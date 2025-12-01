use indexmap::IndexSet;
use runloop_core::AgentRef;
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue};
use serde_yaml::{Mapping, Sequence, Value};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocation {
    pub line: usize,
    pub column: usize,
}

impl SourceLocation {
    const fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("YAML parse error at {line}:{column}: {message}")]
    Parse {
        message: String,
        line: usize,
        column: usize,
    },
    #[error("validation error at {line}:{column}: {message}")]
    Validation {
        message: String,
        line: usize,
        column: usize,
    },
}

impl Error {
    fn parse(err: serde_yaml::Error) -> Self {
        let message = err.to_string();
        let (line, column) = err
            .location()
            .map(|loc| (loc.line(), loc.column()))
            .unwrap_or((0, 0));
        Self::Parse {
            message,
            line: line.saturating_add(1),
            column: column.saturating_add(1),
        }
    }

    fn validation<S: Into<String>>(message: S, location: SourceLocation) -> Self {
        Self::Validation {
            message: message.into(),
            line: location.line.max(1),
            column: location.column.max(1),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Policy {
    pub budget_tokens: Option<u32>,
    pub timeout_ms: Option<u64>,
    pub confirm_external: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NodeKind {
    Agent { reference: AgentRef },
    Opening { name: String },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Retry {
    pub max_attempts: u32,
    pub initial_backoff_ms: u64,
    pub max_backoff_ms: Option<u64>,
    pub multiplier: f32,
    pub jitter: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub with: JsonMap<String, JsonValue>,
    pub schema_hints: SchemaHints,
    pub retry: Retry,
    pub timeout_ms: Option<u64>,
    pub budget_tokens: Option<u32>,
    pub tags: Vec<String>,
    pub location: SourceLocation,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SchemaHints {
    pub with: Option<SchemaHintFragment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SchemaHintFragment {
    Raw(JsonValue),
    Properties(JsonMap<String, JsonValue>),
}

impl SchemaHints {
    /// Convert the hint into a JSON Schema fragment, if present.
    pub fn with_schema(&self) -> Option<JsonValue> {
        match &self.with {
            Some(SchemaHintFragment::Raw(value)) => Some(value.clone()),
            Some(SchemaHintFragment::Properties(props)) => Some(build_hint_schema(props)),
            None => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortReference {
    pub node: String,
    pub port: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Literal {
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComparisonOp {
    Eq,
    NotEq,
    Gt,
    Gte,
    Lt,
    Lte,
}

impl fmt::Display for ComparisonOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let symbol = match self {
            ComparisonOp::Eq => "==",
            ComparisonOp::NotEq => "!=",
            ComparisonOp::Gt => ">",
            ComparisonOp::Gte => ">=",
            ComparisonOp::Lt => "<",
            ComparisonOp::Lte => "<=",
        };
        f.write_str(symbol)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Predicate {
    pub op: ComparisonOp,
    pub value: Literal,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortPredicate {
    pub reference: PortReference,
    pub predicate: Predicate,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Edge {
    pub from: PortReference,
    pub predicate: Option<Predicate>,
    pub to: PortReference,
    pub location: SourceLocation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Expression {
    Exists(PortReference),
    Comparison(PortPredicate),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SuccessCondition {
    AnyOf(Vec<Expression>),
    AllOf(Vec<Expression>),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ArtifactsSpec {
    pub save: Vec<PortReference>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Opening {
    pub version: u32,
    pub name: String,
    pub goals: Vec<String>,
    pub params: JsonMap<String, JsonValue>,
    pub policy: Policy,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub success: Option<SuccessCondition>,
    pub artifacts: ArtifactsSpec,
}

impl Opening {
    /// Return the unique list of agent references referenced by this opening.
    pub fn agent_refs(&self) -> Vec<AgentRef> {
        let mut seen = BTreeSet::new();
        let mut refs = Vec::new();
        for node in &self.nodes {
            if let NodeKind::Agent { reference } = &node.kind
                && seen.insert(reference.clone())
            {
                refs.push(reference.clone());
            }
        }
        refs
    }
}

pub fn parse_opening_str(source: &str) -> Result<Opening, Error> {
    let top: Value = serde_yaml::from_str(source).map_err(Error::parse)?;
    let mapping = top.as_mapping().ok_or_else(|| {
        Error::validation("opening must be a YAML mapping", SourceLocation::new(1, 1))
    })?;

    let version = expect_u32(mapping, "version").ok_or_else(|| {
        Error::validation(
            "missing required field 'version'",
            SourceLocation::new(1, 1),
        )
    })?;
    if version != 0 {
        return Err(Error::validation(
            format!("unsupported DSL version {version}; expected 0"),
            locate_key(source, "version"),
        ));
    }

    let name = expect_string(mapping, "name").ok_or_else(|| {
        Error::validation("missing required field 'name'", SourceLocation::new(1, 1))
    })?;

    let goals = expect_string_list(mapping, "goals", source)?.unwrap_or_default();

    let params_value = mapping_get(mapping, "params");
    let params = match params_value {
        Some(value) => {
            let loc = locate_key(source, "params");
            let map = value
                .as_mapping()
                .ok_or_else(|| Error::validation("params must be a mapping", loc))?;
            yaml_map_to_json_map(map, loc)?
        }
        None => JsonMap::new(),
    };

    let policy = parse_policy(mapping_get(mapping, "policy"), source)?;

    let nodes = parse_nodes(
        mapping_get(mapping, "nodes").ok_or_else(|| {
            Error::validation(
                "missing required field 'nodes'",
                locate_key(source, "nodes"),
            )
        })?,
        source,
        &params,
        policy.confirm_external.unwrap_or(false),
    )?;

    let edges = parse_edges(
        mapping_get(mapping, "edges").ok_or_else(|| {
            Error::validation(
                "missing required field 'edges'",
                locate_key(source, "edges"),
            )
        })?,
        source,
    )?;

    validate_graph(&nodes, &edges, source)?;

    let success = parse_success(mapping_get(mapping, "success"), source)?;

    let artifacts = parse_artifacts(mapping_get(mapping, "artifacts"), source)?;

    Ok(Opening {
        version,
        name,
        goals,
        params,
        policy,
        nodes,
        edges,
        success,
        artifacts,
    })
}

fn parse_policy(value: Option<&Value>, source: &str) -> Result<Policy, Error> {
    let Some(value) = value else {
        return Ok(Policy::default());
    };
    let loc = locate_key(source, "policy");
    let mapping = value
        .as_mapping()
        .ok_or_else(|| Error::validation("policy must be a mapping", loc))?;
    let mut policy = Policy::default();

    if let Some(budget) = mapping_get(mapping, "budget_tokens") {
        let budget_loc = locate_child(source, loc, "budget_tokens");
        let number = expect_u32_value(budget).ok_or_else(|| {
            Error::validation(
                "policy.budget_tokens must be a non-negative integer",
                budget_loc,
            )
        })?;
        policy.budget_tokens = Some(number);
    }

    if let Some(timeout) = mapping_get(mapping, "timeout_ms") {
        let timeout_loc = locate_child(source, loc, "timeout_ms");
        let number = expect_u64_value(timeout).ok_or_else(|| {
            Error::validation(
                "policy.timeout_ms must be a non-negative integer",
                timeout_loc,
            )
        })?;
        policy.timeout_ms = Some(number);
    }

    if let Some(confirm) = mapping_get(mapping, "confirm_external") {
        let confirm_loc = locate_child(source, loc, "confirm_external");
        let value = confirm.as_bool().ok_or_else(|| {
            Error::validation("policy.confirm_external must be a boolean", confirm_loc)
        })?;
        policy.confirm_external = Some(value);
    }

    Ok(policy)
}

fn parse_nodes(
    value: &Value,
    source: &str,
    params: &JsonMap<String, JsonValue>,
    confirm_external: bool,
) -> Result<Vec<Node>, Error> {
    let loc = locate_key(source, "nodes");
    let seq = value
        .as_sequence()
        .ok_or_else(|| Error::validation("nodes must be a sequence", loc))?;
    if seq.is_empty() {
        return Err(Error::validation(
            "nodes must contain at least one entry",
            loc,
        ));
    }

    let mut seen = IndexSet::new();
    let mut nodes = Vec::new();

    for entry in seq {
        let node_loc = locate_node(source, entry);
        let mapping = entry
            .as_mapping()
            .ok_or_else(|| Error::validation("node entry must be a mapping", node_loc))?;

        let id = expect_string_in(mapping, "id", source, node_loc)?;
        if !seen.insert(id.clone()) {
            return Err(Error::validation(
                format!("duplicate node id '{id}'"),
                node_loc,
            ));
        }

        let use_value = expect_string_in(mapping, "use", source, node_loc)?;
        let kind = parse_use(&use_value, source, node_loc)?;

        let with_map = mapping_get(mapping, "with")
            .map(|v| -> Result<JsonMap<String, JsonValue>, Error> {
                let with_loc = locate_child(source, node_loc, "with");
                let mapping = v
                    .as_mapping()
                    .ok_or_else(|| Error::validation("node.with must be a mapping", with_loc))?;
                yaml_map_to_json_map(mapping, with_loc)
            })
            .transpose()?
            .unwrap_or_default();

        let mut with = with_map;
        apply_param_templates(&mut with, params, locate_child(source, node_loc, "with"))?;
        let schema_hints =
            parse_schema_hints(mapping_get(mapping, "schema_hints"), source, node_loc)?;

        if confirm_external
            && matches!(
                &kind,
                NodeKind::Agent { reference } if reference.name == "mailer"
            )
            && matches!(
                with.get("require_human_confirm"),
                Some(JsonValue::Bool(false))
            )
        {
            return Err(Error::validation(
                "policy.confirm_external=true forbids nodes from disabling confirmation",
                locate_child(source, node_loc, "with"),
            ));
        }

        let retry = mapping_get(mapping, "retry")
            .map(|v| parse_retry(v, source, node_loc))
            .transpose()?
            .unwrap_or_default();

        let timeout_ms = mapping_get(mapping, "timeout_ms")
            .map(|value| {
                let timeout_loc = locate_child(source, node_loc, "timeout_ms");
                expect_u64_value(value).ok_or_else(|| {
                    Error::validation("timeout_ms must be a non-negative integer", timeout_loc)
                })
            })
            .transpose()?;

        let budget_tokens = mapping_get(mapping, "budget_tokens")
            .map(|value| {
                let budget_loc = locate_child(source, node_loc, "budget_tokens");
                expect_u32_value(value).ok_or_else(|| {
                    Error::validation("budget_tokens must be a non-negative integer", budget_loc)
                })
            })
            .transpose()?;

        let tags = mapping_get(mapping, "tags")
            .map(|value| {
                let tag_loc = locate_child(source, node_loc, "tags");
                let seq = value.as_sequence().ok_or_else(|| {
                    Error::validation("tags must be a sequence of strings", tag_loc)
                })?;
                let mut tags = Vec::with_capacity(seq.len());
                for tag in seq {
                    let Some(tag_str) = tag.as_str() else {
                        return Err(Error::validation("tags entries must be strings", tag_loc));
                    };
                    tags.push(tag_str.to_string());
                }
                Ok(tags)
            })
            .transpose()?
            .unwrap_or_default();

        nodes.push(Node {
            id,
            kind,
            with,
            schema_hints,
            retry,
            timeout_ms,
            budget_tokens,
            tags,
            location: node_loc,
        });
    }

    Ok(nodes)
}

fn parse_schema_hints(
    value: Option<&Value>,
    source: &str,
    node_loc: SourceLocation,
) -> Result<SchemaHints, Error> {
    let Some(value) = value else {
        return Ok(SchemaHints::default());
    };
    let loc = locate_child(source, node_loc, "schema_hints");
    let mapping = value
        .as_mapping()
        .ok_or_else(|| Error::validation("schema_hints must be a mapping", loc))?;
    let with = mapping_get(mapping, "with")
        .map(|v| parse_with_hint_fragment(v, source, locate_child(source, loc, "with")))
        .transpose()?;
    Ok(SchemaHints { with })
}

fn parse_with_hint_fragment(
    value: &Value,
    _source: &str,
    loc: SourceLocation,
) -> Result<SchemaHintFragment, Error> {
    if let Some(map) = value.as_mapping() {
        if is_schema_like(map) {
            let json_value: JsonValue = serde_yaml::from_value(value.clone()).map_err(|err| {
                Error::validation(format!("invalid schema_hints.with entry: {err}"), loc)
            })?;
            return Ok(SchemaHintFragment::Raw(json_value));
        }
        let mut props = JsonMap::new();
        for (key, fragment) in map {
            let Some(key_str) = key.as_str() else {
                return Err(Error::validation(
                    "schema_hints.with keys must be strings",
                    loc,
                ));
            };
            let json_value: JsonValue =
                serde_yaml::from_value(fragment.clone()).map_err(|err| {
                    Error::validation(format!("invalid schema hint for '{key_str}': {err}"), loc)
                })?;
            props.insert(key_str.to_string(), json_value);
        }
        return Ok(SchemaHintFragment::Properties(props));
    }
    let json_value: JsonValue = serde_yaml::from_value(value.clone())
        .map_err(|err| Error::validation(format!("invalid schema_hints.with value: {err}"), loc))?;
    Ok(SchemaHintFragment::Raw(json_value))
}

fn is_schema_like(map: &Mapping) -> bool {
    const SCHEMA_KEYS: &[&str] = &[
        "type",
        "$schema",
        "properties",
        "required",
        "allOf",
        "anyOf",
        "oneOf",
        "not",
        "items",
        "const",
        "$defs",
        "definitions",
        "$ref",
    ];
    map.keys().any(|key| {
        key.as_str()
            .map(|candidate| {
                SCHEMA_KEYS
                    .iter()
                    .any(|schema_key| schema_key == &candidate)
            })
            .unwrap_or(false)
    })
}

fn build_hint_schema(props: &JsonMap<String, JsonValue>) -> JsonValue {
    let mut required = Vec::new();
    let mut normalized = JsonMap::new();
    for (name, fragment) in props {
        let mut is_required = false;
        let cleaned = match fragment.clone() {
            JsonValue::Object(mut obj) => {
                if let Some(flag) = obj.remove("required")
                    && flag.as_bool().unwrap_or(false)
                {
                    is_required = true;
                }
                JsonValue::Object(obj)
            }
            other => other,
        };
        if is_required {
            required.push(JsonValue::String(name.clone()));
        }
        normalized.insert(name.clone(), cleaned);
    }
    let mut schema = JsonMap::new();
    schema.insert("type".into(), JsonValue::String("object".into()));
    schema.insert("properties".into(), JsonValue::Object(normalized));
    if !required.is_empty() {
        schema.insert("required".into(), JsonValue::Array(required));
    }
    JsonValue::Object(schema)
}

fn parse_use(raw: &str, source: &str, loc: SourceLocation) -> Result<NodeKind, Error> {
    let parts: Vec<_> = raw.split(':').collect();
    if parts.len() != 2 {
        return Err(Error::validation(
            "node.use must be of the form 'agent:<name>' or 'opening:<name>'",
            loc,
        ));
    }
    let kind = match parts[0] {
        "agent" => {
            let reference = parse_agent_reference(parts[1], source, loc)?;
            NodeKind::Agent { reference }
        }
        "opening" => NodeKind::Opening {
            name: parts[1].to_string(),
        },
        other => {
            return Err(Error::validation(
                format!("unknown use prefix '{other}'"),
                locate_pattern(source, format!("use: {raw}")),
            ));
        }
    };
    Ok(kind)
}

fn parse_agent_reference(raw: &str, source: &str, loc: SourceLocation) -> Result<AgentRef, Error> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error::validation("agent reference cannot be empty", loc));
    }
    let (name, variant) = if let Some((name, variant)) = trimmed.split_once('@') {
        (name.trim(), Some(variant.trim()))
    } else {
        (trimmed, None)
    };
    if name.is_empty() {
        return Err(Error::validation("agent name cannot be empty", loc));
    }
    if !valid_identifier(name) {
        return Err(Error::validation(
            "agent name must contain only alphanumeric characters, '-' or '_'",
            locate_pattern(source, format!("use: agent:{raw}")),
        ));
    }
    if let Some(variant) = variant {
        if variant.is_empty() {
            return Err(Error::validation("agent variant cannot be empty", loc));
        }
        if !valid_identifier(variant) {
            return Err(Error::validation(
                "agent variant must contain only alphanumeric characters, '-' or '_'",
                locate_pattern(source, format!("use: agent:{raw}")),
            ));
        }
        return Ok(AgentRef::new(name.to_string(), Some(variant.to_string())));
    }
    Ok(AgentRef::new(name.to_string(), None))
}

fn valid_identifier(value: &str) -> bool {
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn parse_retry(value: &Value, source: &str, node_loc: SourceLocation) -> Result<Retry, Error> {
    let loc = locate_child(source, node_loc, "retry");
    let mapping = value
        .as_mapping()
        .ok_or_else(|| Error::validation("retry must be a mapping", loc))?;
    let mut retry = Retry {
        max_attempts: 0,
        initial_backoff_ms: 100,
        max_backoff_ms: None,
        multiplier: 2.0,
        jitter: 0.2,
    };

    if let Some(value) = mapping_get(mapping, "max_attempts") {
        let attempts = expect_u32_value(value).ok_or_else(|| {
            Error::validation("retry.max_attempts must be a non-negative integer", loc)
        })?;
        retry.max_attempts = attempts;
    }

    if let Some(value) = mapping_get(mapping, "backoff_ms") {
        let backoff = expect_u64_value(value).ok_or_else(|| {
            Error::validation("retry.backoff_ms must be a non-negative integer", loc)
        })?;
        retry.initial_backoff_ms = backoff;
    }

    if let Some(value) = mapping_get(mapping, "initial_ms") {
        let initial = expect_u64_value(value).ok_or_else(|| {
            Error::validation("retry.initial_ms must be a non-negative integer", loc)
        })?;
        retry.initial_backoff_ms = initial;
    }

    if let Some(value) = mapping_get(mapping, "max_ms") {
        let max = expect_u64_value(value)
            .ok_or_else(|| Error::validation("retry.max_ms must be a non-negative integer", loc))?;
        retry.max_backoff_ms = Some(max);
    }

    if let Some(value) = mapping_get(mapping, "multiplier") {
        let multiplier = value.as_f64().ok_or_else(|| {
            Error::validation("retry.multiplier must be a floating point number", loc)
        })?;
        retry.multiplier = multiplier as f32;
    }

    if let Some(value) = mapping_get(mapping, "jitter") {
        let jitter = value.as_f64().ok_or_else(|| {
            Error::validation("retry.jitter must be a floating point number", loc)
        })?;
        if !(0.0..=1.0).contains(&jitter) {
            return Err(Error::validation(
                "retry.jitter must be between 0.0 and 1.0",
                loc,
            ));
        }
        retry.jitter = jitter as f32;
    }

    Ok(retry)
}

fn parse_edges(value: &Value, source: &str) -> Result<Vec<Edge>, Error> {
    let loc = locate_key(source, "edges");
    let seq = value
        .as_sequence()
        .ok_or_else(|| Error::validation("edges must be a sequence", loc))?;

    let mut edges = Vec::with_capacity(seq.len());
    for entry in seq {
        let edge_loc = locate_edge(source, entry);
        if let Some(mapping) = entry.as_mapping() {
            let from_raw = expect_string_in(mapping, "from", source, edge_loc)?;
            let to_raw = expect_string_in(mapping, "to", source, edge_loc)?;
            let (from_ref, predicate) = parse_port_predicate(&from_raw, edge_loc)?;
            let to_ref = parse_port_reference(&to_raw, edge_loc)?;
            edges.push(Edge {
                from: from_ref,
                predicate,
                to: to_ref,
                location: edge_loc,
            });
        } else {
            return Err(Error::validation(
                "edge entries must be mappings with 'from' and 'to'",
                edge_loc,
            ));
        }
    }

    Ok(edges)
}

fn parse_success(value: Option<&Value>, source: &str) -> Result<Option<SuccessCondition>, Error> {
    let Some(value) = value else {
        return Ok(None);
    };
    let loc = locate_key(source, "success");
    let mapping = value
        .as_mapping()
        .ok_or_else(|| Error::validation("success must be a mapping", loc))?;

    let any = mapping_get(mapping, "any_of");
    let all = mapping_get(mapping, "all_of");
    if any.is_some() && all.is_some() {
        return Err(Error::validation(
            "success must define either 'any_of' or 'all_of', not both",
            loc,
        ));
    }

    if let Some(seq_value) = any {
        let seq = seq_value
            .as_sequence()
            .ok_or_else(|| Error::validation("success.any_of must be a sequence", loc))?;
        let expressions = parse_expression_list(seq, loc)?;
        return Ok(Some(SuccessCondition::AnyOf(expressions)));
    }

    if let Some(seq_value) = all {
        let seq = seq_value
            .as_sequence()
            .ok_or_else(|| Error::validation("success.all_of must be a sequence", loc))?;
        let expressions = parse_expression_list(seq, loc)?;
        return Ok(Some(SuccessCondition::AllOf(expressions)));
    }

    Err(Error::validation(
        "success must specify 'any_of' or 'all_of'",
        loc,
    ))
}

fn parse_expression_list(seq: &Sequence, loc: SourceLocation) -> Result<Vec<Expression>, Error> {
    if seq.is_empty() {
        return Err(Error::validation(
            "success expressions must not be empty",
            loc,
        ));
    }
    let mut expressions = Vec::with_capacity(seq.len());
    for expr_value in seq {
        let Some(expr_str) = expr_value.as_str() else {
            return Err(Error::validation(
                "success expressions must be strings",
                loc,
            ));
        };
        expressions.push(parse_expression(expr_str, loc)?);
    }
    Ok(expressions)
}

fn parse_expression(raw: &str, loc: SourceLocation) -> Result<Expression, Error> {
    if let Some(inner) = raw
        .strip_prefix("exists(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let reference = parse_port_reference(inner.trim(), loc)?;
        return Ok(Expression::Exists(reference));
    }

    let (reference, predicate) = parse_port_predicate(raw, loc)?;
    Ok(Expression::Comparison(PortPredicate {
        reference,
        predicate: predicate.ok_or_else(|| {
            Error::validation(
                "comparison expression must include an operator (==, !=, >, >=, <, <=)",
                loc,
            )
        })?,
    }))
}

fn parse_artifacts(value: Option<&Value>, source: &str) -> Result<ArtifactsSpec, Error> {
    let Some(value) = value else {
        return Ok(ArtifactsSpec::default());
    };
    let loc = locate_key(source, "artifacts");
    let mapping = value
        .as_mapping()
        .ok_or_else(|| Error::validation("artifacts must be a mapping", loc))?;

    let mut artifacts = ArtifactsSpec::default();

    if let Some(save_value) = mapping_get(mapping, "save") {
        let save_loc = locate_child(source, loc, "save");
        let seq = save_value
            .as_sequence()
            .ok_or_else(|| Error::validation("artifacts.save must be a sequence", save_loc))?;
        for (idx, entry) in seq.iter().enumerate() {
            let Some(spec) = entry.as_str() else {
                return Err(Error::validation(
                    format!("artifacts.save entry {idx} must be a string"),
                    save_loc,
                ));
            };
            let port = parse_port_reference(spec, save_loc)?;
            artifacts.save.push(port);
        }
    }

    Ok(artifacts)
}

fn mapping_get<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a Value> {
    let key_value = Value::from(key);
    mapping.get(&key_value)
}

fn parse_port_predicate(
    raw: &str,
    loc: SourceLocation,
) -> Result<(PortReference, Option<Predicate>), Error> {
    let operators = [
        (">=", ComparisonOp::Gte),
        ("<=", ComparisonOp::Lte),
        ("==", ComparisonOp::Eq),
        ("!=", ComparisonOp::NotEq),
        (">", ComparisonOp::Gt),
        ("<", ComparisonOp::Lt),
    ];

    for (symbol, op) in operators {
        if let Some((left, right)) = split_once(raw, symbol) {
            let reference = parse_port_reference(left.trim(), loc)?;
            let literal = parse_literal(right.trim(), loc)?;
            return Ok((reference, Some(Predicate { op, value: literal })));
        }
    }

    let reference = parse_port_reference(raw.trim(), loc)?;
    Ok((reference, None))
}

fn parse_port_reference(raw: &str, loc: SourceLocation) -> Result<PortReference, Error> {
    let trimmed = raw.trim();
    let Some((node, port)) = trimmed.split_once('.') else {
        return Err(Error::validation(
            "port reference must be of the form '<node>.<port>'",
            loc,
        ));
    };
    if node.is_empty() || port.is_empty() {
        return Err(Error::validation(
            "node and port identifiers must be non-empty",
            loc,
        ));
    }
    Ok(PortReference {
        node: node.to_string(),
        port: port.to_string(),
    })
}

fn parse_literal(raw: &str, loc: SourceLocation) -> Result<Literal, Error> {
    if let Ok(value) = raw.parse::<bool>() {
        return Ok(Literal::Bool(value));
    }
    if let Ok(value) = raw.parse::<i64>() {
        return Ok(Literal::Integer(value));
    }
    if let Ok(value) = raw.parse::<f64>() {
        return Ok(Literal::Float(value));
    }
    if let Some(stripped) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return Ok(Literal::String(stripped.to_string()));
    }
    Err(Error::validation(
        "unsupported literal value; use booleans, numbers, or quoted strings",
        loc,
    ))
}

fn validate_graph(nodes: &[Node], edges: &[Edge], source: &str) -> Result<(), Error> {
    let mut id_to_index = HashMap::new();
    for (idx, node) in nodes.iter().enumerate() {
        id_to_index.insert(node.id.as_str(), idx);
    }

    for edge in edges {
        if !id_to_index.contains_key(edge.from.node.as_str()) {
            return Err(Error::validation(
                format!("edge references unknown node '{}'", edge.from.node),
                edge.location,
            ));
        }
        if !id_to_index.contains_key(edge.to.node.as_str()) {
            return Err(Error::validation(
                format!("edge references unknown node '{}'", edge.to.node),
                edge.location,
            ));
        }
    }

    let mut indegree = vec![0usize; nodes.len()];
    let mut adjacency = vec![Vec::<usize>::new(); nodes.len()];
    for edge in edges {
        let from_idx = *id_to_index
            .get(edge.from.node.as_str())
            .expect("validated above");
        let to_idx = *id_to_index
            .get(edge.to.node.as_str())
            .expect("validated above");
        adjacency[from_idx].push(to_idx);
        indegree[to_idx] += 1;
    }

    let mut queue = VecDeque::new();
    for (idx, &deg) in indegree.iter().enumerate() {
        if deg == 0 {
            queue.push_back(idx);
        }
    }

    let mut visited = 0usize;
    while let Some(idx) = queue.pop_front() {
        visited += 1;
        for &successor in &adjacency[idx] {
            indegree[successor] -= 1;
            if indegree[successor] == 0 {
                queue.push_back(successor);
            }
        }
    }

    if visited != nodes.len() {
        return Err(Error::validation(
            "opening graph contains a cycle; ensure edges form a DAG",
            locate_key(source, "edges"),
        ));
    }

    Ok(())
}

fn expect_u32(mapping: &Mapping, key: &str) -> Option<u32> {
    mapping_get(mapping, key).and_then(|value| {
        if let Some(raw) = value.as_i64() {
            if raw >= 0 {
                return raw.try_into().ok();
            }
            return None;
        }
        value.as_u64().and_then(|v| u32::try_from(v).ok())
    })
}

fn expect_string(mapping: &Mapping, key: &str) -> Option<String> {
    mapping_get(mapping, key).and_then(|value| value.as_str().map(|s| s.to_string()))
}

fn expect_string_list(
    mapping: &Mapping,
    key: &str,
    source: &str,
) -> Result<Option<Vec<String>>, Error> {
    let Some(value) = mapping_get(mapping, key) else {
        return Ok(None);
    };
    let loc = locate_key(source, key);
    let seq = value
        .as_sequence()
        .ok_or_else(|| Error::validation(format!("{key} must be a sequence"), loc))?;
    let mut items = Vec::with_capacity(seq.len());
    for (idx, entry) in seq.iter().enumerate() {
        let Some(text) = entry.as_str() else {
            return Err(Error::validation(
                format!("{key}[{idx}] must be a string"),
                loc,
            ));
        };
        items.push(text.to_string());
    }
    Ok(Some(items))
}

fn expect_string_in(
    mapping: &Mapping,
    key: &str,
    source: &str,
    fallback_loc: SourceLocation,
) -> Result<String, Error> {
    let Some(value) = mapping_get(mapping, key) else {
        return Err(Error::validation(
            format!("missing required field '{key}'"),
            locate_child(source, fallback_loc, key),
        ));
    };
    value.as_str().map(|s| s.to_string()).ok_or_else(|| {
        Error::validation(
            format!("field '{key}' must be a string"),
            locate_child(source, fallback_loc, key),
        )
    })
}

fn expect_u32_value(value: &Value) -> Option<u32> {
    value
        .as_i64()
        .and_then(|raw| if raw >= 0 { raw.try_into().ok() } else { None })
        .or_else(|| value.as_u64().and_then(|v| v.try_into().ok()))
}

fn expect_u64_value(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_i64()
            .and_then(|i| if i >= 0 { Some(i as u64) } else { None })
    })
}

fn yaml_map_to_json_map(
    map: &Mapping,
    loc: SourceLocation,
) -> Result<JsonMap<String, JsonValue>, Error> {
    let mut json_map = JsonMap::new();
    for (key, value) in map {
        let Some(key_str) = key.as_str() else {
            return Err(Error::validation("parameter keys must be strings", loc));
        };
        let json_value: JsonValue = serde_yaml::from_value(value.clone()).map_err(|err| {
            Error::validation(
                format!("unsupported value type for key '{key_str}': {err}"),
                loc,
            )
        })?;
        // params_map previously restricted values to scalars because templating only
        // supported scalar substitution. Now that we allow full-value replacement,
        // we accept any JSON type here and rely on downstream schema/agent validation.
        json_map.insert(key_str.to_string(), json_value);
    }
    Ok(json_map)
}

fn apply_param_templates(
    payload: &mut JsonMap<String, JsonValue>,
    params: &JsonMap<String, JsonValue>,
    loc: SourceLocation,
) -> Result<(), Error> {
    for value in payload.values_mut() {
        match value {
            JsonValue::String(text) => {
                if let Some(param_name) = extract_template(text) {
                    let param = params.get(param_name).ok_or_else(|| {
                        Error::validation(
                            format!("template '{{{{params.{param_name}}}}}' references missing parameter"),
                            loc,
                        )
                    })?;
                    validate_no_templates(param, loc)?;
                    *value = param.clone();
                } else if text.contains("{{") || text.contains("}}") {
                    return Err(Error::validation(
                        format!(
                            "unsupported template syntax in value '{text}'; only '{{{{params.*}}}}' is allowed"
                        ),
                        loc,
                    ));
                }
            }
            JsonValue::Object(map) => apply_param_templates(map, params, loc)?,
            JsonValue::Array(_) => apply_templates_array(value, params, loc)?,
            _ => {}
        }
    }
    Ok(())
}

fn apply_templates_array(
    value: &mut JsonValue,
    params: &JsonMap<String, JsonValue>,
    loc: SourceLocation,
) -> Result<(), Error> {
    if let JsonValue::Array(arr) = value {
        for element in arr.iter_mut() {
            match element {
                JsonValue::Object(map) => apply_param_templates(map, params, loc)?,
                JsonValue::Array(_) => apply_templates_array(element, params, loc)?,
                JsonValue::String(text) => {
                    if let Some(param_name) = extract_template(text) {
                        let param = params.get(param_name).ok_or_else(|| {
                            Error::validation(
                                format!(
                                    "template '{{{{params.{param_name}}}}}' references missing parameter"
                                ),
                                loc,
                            )
                        })?;
                        validate_no_templates(param, loc)?;
                        *element = param.clone();
                    } else if text.contains("{{") || text.contains("}}") {
                        return Err(Error::validation(
                            format!(
                                "unsupported template syntax in value '{text}'; only '{{{{params.*}}}}' is allowed"
                            ),
                            loc,
                        ));
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn extract_template(value: &str) -> Option<&str> {
    let rest = value
        .strip_prefix("{{")
        .and_then(|s| s.strip_suffix("}}"))
        .map(str::trim)?
        .strip_prefix("params.")?;
    if rest
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        Some(rest)
    } else {
        None
    }
}

fn validate_no_templates(value: &JsonValue, loc: SourceLocation) -> Result<(), Error> {
    match value {
        JsonValue::String(text) => {
            if text.contains("{{") || text.contains("}}") {
                return Err(Error::validation(
                    format!("embedded template syntax not allowed in parameter value '{text}'"),
                    loc,
                ));
            }
        }
        JsonValue::Array(arr) => {
            for item in arr {
                validate_no_templates(item, loc)?;
            }
        }
        JsonValue::Object(map) => {
            for value in map.values() {
                validate_no_templates(value, loc)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn split_once<'a>(haystack: &'a str, needle: &str) -> Option<(&'a str, &'a str)> {
    haystack.split_once(needle)
}

fn locate_key(source: &str, key: &str) -> SourceLocation {
    locate_pattern(source, format!("{key}:"))
}

fn locate_child(source: &str, _parent: SourceLocation, key: &str) -> SourceLocation {
    let pattern = format!("{key}:");
    locate_pattern(source, pattern)
}

fn locate_node(source: &str, value: &Value) -> SourceLocation {
    if let Some(id) = value
        .as_mapping()
        .and_then(|mapping| mapping_get(mapping, "id"))
        .and_then(Value::as_str)
    {
        locate_pattern(source, format!("id: {id}"))
    } else {
        locate_key(source, "nodes")
    }
}

fn locate_edge(source: &str, value: &Value) -> SourceLocation {
    if let Some(raw) = value
        .as_mapping()
        .and_then(|mapping| mapping_get(mapping, "from"))
        .and_then(Value::as_str)
    {
        locate_pattern(source, format!("from: {raw}"))
    } else {
        locate_key(source, "edges")
    }
}

fn locate_pattern(source: &str, pattern: String) -> SourceLocation {
    if let Some(index) = locate_index(source, &pattern) {
        index_to_location(source, index)
    } else {
        SourceLocation::new(1, 1)
    }
}

fn locate_index(source: &str, pattern: &str) -> Option<usize> {
    source.find(pattern)
}

fn index_to_location(source: &str, index: usize) -> SourceLocation {
    let mut line = 1usize;
    let mut column = 1usize;
    for (idx, ch) in source.chars().enumerate() {
        if idx == index {
            break;
        }
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    SourceLocation::new(line, column)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn applies_array_param_when_template_is_entire_value() {
        let mut payload = JsonMap::new();
        payload.insert(
            "extra_lines".into(),
            JsonValue::String("{{params.extra_lines}}".into()),
        );

        let mut params = JsonMap::new();
        params.insert(
            "extra_lines".into(),
            json!(["set -g mouse on", "setw -g mode-keys vi"]),
        );

        apply_param_templates(&mut payload, &params, SourceLocation::new(1, 1))
            .expect("should substitute array param");

        assert_eq!(
            payload.get("extra_lines"),
            Some(&json!(["set -g mouse on", "setw -g mode-keys vi"]))
        );
    }

    #[test]
    fn applies_object_param_inside_array_element() {
        let mut payload = JsonMap::new();
        payload.insert(
            "items".into(),
            JsonValue::Array(vec![JsonValue::String("{{params.obj}}".into())]),
        );

        let mut params = JsonMap::new();
        params.insert("obj".into(), json!({"key": "value"}));

        apply_param_templates(&mut payload, &params, SourceLocation::new(1, 1))
            .expect("should substitute object param inside array");

        let items = payload
            .get("items")
            .and_then(|v| v.as_array())
            .expect("items array");
        assert_eq!(items.get(0), Some(&json!({"key": "value"})));
    }

    #[test]
    fn rejects_inline_template_with_non_scalar_param() {
        let mut payload = JsonMap::new();
        payload.insert(
            "line".into(),
            JsonValue::String("prefix {{params.extra_lines}}".into()),
        );

        let mut params = JsonMap::new();
        params.insert("extra_lines".into(), json!(["foo"]));

        let err = apply_param_templates(&mut payload, &params, SourceLocation::new(3, 5))
            .expect_err("inline template should be rejected");

        assert!(matches!(err, Error::Validation { .. }));
    }

    #[test]
    fn allows_templates_inside_object_values() {
        let mut payload = JsonMap::new();
        payload.insert(
            "input".into(),
            json!({
                "history_limit": "{{params.limit}}",
                "tmux_conf": "~/.tmux.conf"
            }),
        );

        let mut params = JsonMap::new();
        params.insert("limit".into(), json!(1234));

        apply_param_templates(&mut payload, &params, SourceLocation::new(1, 1))
            .expect("object template should be substituted");

        let input = payload
            .get("input")
            .and_then(|v| v.as_object())
            .expect("input object");
        assert_eq!(input.get("history_limit"), Some(&json!(1234)));
        assert_eq!(input.get("tmux_conf"), Some(&json!("~/.tmux.conf")));
    }
}
