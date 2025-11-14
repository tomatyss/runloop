mod output;
mod run_events;

use async_trait::async_trait;
use bytes::Bytes;
use clap::{Args, Parser, Subcommand};
use dirs::home_dir;
use futures_util::StreamExt;
use output::{
    Cell, OutputArgs, OutputMode, Table, display_value, print_json, print_table, summarize_json,
};
use run_events::{RunEventEmitter, emit_ndjson_from_run_event_env};
use runloop_agents_common::{
    ActionDecision, ActionProposal, AgentError, AgentResult, ConfirmationProvider,
};
use runloop_bus::Bus;
use runloop_core::config::{ConfigLayer, ConfigSource};
use runloop_core::content::{CT_CTRL_REQ, CT_CTRL_RESP, CT_RUN_EVENT};
use runloop_core::{Config, OpeningId, TraceId};
use runloop_core::{ControlRequest, ControlResponse, RunAccepted, RunSubmitRequest};
use runloop_executor_local::{
    ExecutorInitError, LocalExecutor, build_executor as build_local_executor, catch_up_views,
};
use runloop_kb::{EventRecord, KnowledgeBase, Materializer, Provenance, StateDelta};
use runloop_model_broker::SecretResolver;
use runloop_openings::{
    Opening, ReplayMismatch, RunReport, RunTrace, Runner, RunnerError, parse_opening_str, replay,
};
use runloop_rmp::{Header, decode_payload, encode_payload};
use runloop_router::Router;
use serde_json::{Value as JsonValue, json, to_string_pretty, to_value, to_writer_pretty};
use serde_yaml::{self, Mapping as YamlMapping, Value as YamlValue};
use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::{fs, fs::File};
use thiserror::Error;
use tokio::time::Duration;
use tokio::{net::UnixStream, sync::mpsc, task, time};
use uuid::Uuid;

const CLI_AGENT_ID: &str = "agent:rlp-cli";

#[derive(Parser, Debug)]
#[command(name = "rlp", about = "Runloop CLI (work in progress)")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Why(WhyArgs),
    Run(RunArgs),
    Replay(ReplayArgs),
    #[command(subcommand)]
    Kb(KbCommands),
    #[command(subcommand)]
    Config(ConfigCommands),
}

#[derive(Args, Debug)]
struct WhyArgs {
    /// Prompt to explain classification for.
    prompt: String,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Path to the opening YAML file to execute.
    #[arg(value_name = "OPENING_PATH")]
    path: String,
    /// Optional JSON object providing parameter overrides.
    #[arg(long, value_name = "JSON")]
    params: Option<String>,
    /// Optional path to write a serialized run trace for replay.
    #[arg(long, value_name = "TRACE_PATH")]
    trace_out: Option<PathBuf>,
    /// Execute directly via the local executor instead of the daemon.
    #[arg(long)]
    local: bool,
}

#[derive(Args, Debug)]
struct ReplayArgs {
    /// Path to a previously recorded run trace (JSON).
    #[arg(value_name = "TRACE_PATH")]
    trace_path: String,
    /// Opening YAML to use when replaying the trace.
    #[arg(long, value_name = "OPENING_PATH")]
    opening: String,
}

#[derive(Subcommand, Debug)]
enum KbCommands {
    Query(QueryArgs),
    Search(SearchArgs),
    Why(KbWhyArgs),
    Migrate,
}

#[derive(Args, Debug)]
struct QueryArgs {
    /// Query expression for the knowledge base.
    #[arg(value_name = "EXPR", trailing_var_arg = true)]
    expression: Vec<String>,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Args, Debug)]
struct SearchArgs {
    /// Keyword to search for across contacts, artifacts, events, and runs.
    #[arg(value_name = "KEYWORD", trailing_var_arg = true)]
    keyword: Vec<String>,
}

#[derive(Args, Debug)]
struct KbWhyArgs {
    /// Entity key (e.g. contact:<hash>) to inspect provenance for.
    #[arg(value_name = "ENTITY")]
    entity: String,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Subcommand, Debug)]
enum ConfigCommands {
    Path(ConfigPathArgs),
}

#[derive(Args, Debug)]
struct ConfigPathArgs {
    /// Show the full provenance chain instead of just the active file.
    #[arg(long)]
    all: bool,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Debug, Error)]
enum CliError {
    #[error(transparent)]
    Core(#[from] runloop_core::Error),
    #[error(transparent)]
    Rmp(#[from] runloop_rmp::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Kb(#[from] runloop_kb::Error),
    #[error(transparent)]
    Openings(#[from] runloop_openings::Error),
    #[error(transparent)]
    Runner(#[from] RunnerError),
    #[error(transparent)]
    ExecutorInit(#[from] ExecutorInitError),
    #[error("invalid parameters: {0}")]
    InvalidParams(String),
    #[error("invalid trace: {0}")]
    InvalidTrace(String),
    #[error("opening run failed (trace {0})")]
    RunFailure(TraceId),
    #[error("replay detected mismatches")]
    ReplayFailure(Vec<ReplayMismatch>),
    #[error("runloop daemon unavailable; tried {0}")]
    DaemonUnavailable(String),
    #[error("daemon at {0} is unreachable; start it or use --local")]
    DaemonUnreachable(String),
    #[error("control handshake timed out waiting for acceptance")]
    AcceptTimeout,
    #[error("run rejected: {code} — {message}")]
    RunRejected { code: String, message: String },
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), CliError> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Why(args) => handle_why(args).await,
        Commands::Run(args) => handle_run(args).await,
        Commands::Replay(args) => handle_replay(args).await,
        Commands::Kb(cmd) => handle_kb(cmd).await,
        Commands::Config(cmd) => handle_config(cmd).await,
    }
}

async fn handle_why(args: WhyArgs) -> Result<(), CliError> {
    let config = Config::load()?;
    let router = Router::from_config(&config.router);
    let classification = router.classify(&args.prompt);
    let settings = args.output.resolve();
    match settings.mode {
        OutputMode::Json => {
            let value = serde_json::to_value(&classification)?;
            print_json(&value)?;
        }
        OutputMode::Table => {
            let mut table = Table::new(vec![
                "route".into(),
                "rule".into(),
                "blocked".into(),
                "features".into(),
                "reason".into(),
            ]);
            let features = if classification.features.is_empty() {
                "—".into()
            } else {
                classification.features.join(", ")
            };
            table.add_row(vec![
                Cell::text(classification.route.to_string()),
                Cell::text(classification.rule.clone()),
                Cell::text(classification.blocked.to_string()),
                Cell::text(features),
                Cell::text(classification.reason.clone()),
            ]);
            print_table(&table, &settings)?;
        }
    }
    Ok(())
}

async fn handle_run(args: RunArgs) -> Result<(), CliError> {
    let config = Config::load()?;
    let prepared = prepare_opening(&args.path, args.params.as_deref())?;
    if !args.local {
        return submit_via_daemon(&config, &prepared).await;
    }
    run_opening_locally(&config, prepared, args.trace_out.clone()).await
}

async fn handle_replay(args: ReplayArgs) -> Result<(), CliError> {
    let config = Config::load()?;
    let trace_data = fs::read_to_string(&args.trace_path)?;
    let trace: RunTrace =
        serde_json::from_str(&trace_data).map_err(|err| CliError::InvalidTrace(err.to_string()))?;

    let opening_source = fs::read_to_string(&args.opening)?;
    let opening = parse_opening_str(&opening_source)?;

    let executor = build_executor(config)?;
    let report = replay(executor.as_ref(), &opening, &trace).await?;

    println!("trace id: {}", trace.trace_id);
    println!("original success: {}", trace.success);
    println!("replay hash: {}", report.replay_hash);
    if report.matches {
        println!("replay matches recorded outputs");
        Ok(())
    } else {
        println!("replay mismatches detected ({}):", report.mismatches.len());
        for mismatch in &report.mismatches {
            let expected = mismatch.expected_hash.as_deref().unwrap_or("<none>");
            let actual = mismatch.actual_hash.as_deref().unwrap_or("<none>");
            println!(
                "  {} -> {} (expected {}, actual {})",
                mismatch.node_id, mismatch.reason, expected, actual
            );
        }
        Err(CliError::ReplayFailure(report.mismatches.clone()))
    }
}

async fn handle_kb(cmd: KbCommands) -> Result<(), CliError> {
    let config = Config::load()?;
    match cmd {
        KbCommands::Query(args) => {
            let expr = args.expression.join(" ");
            if expr.trim().is_empty() {
                println!("kb query requires a SQL expression");
                return Ok(());
            }
            let kb = KnowledgeBase::open(&config.kb)?;
            catch_up_views(&kb)?;
            let result = kb.query(&expr)?;
            let settings = args.output.resolve();
            render_query_output(&result, &settings)?;
        }
        KbCommands::Search(args) => {
            let keyword = args.keyword.join(" ");
            if keyword.trim().is_empty() {
                println!("kb search requires a keyword");
                return Ok(());
            }
            let kb = KnowledgeBase::open(&config.kb)?;
            catch_up_views(&kb)?;
            let results = kb.search(&keyword)?;
            let rendered = to_string_pretty(&results)?;
            println!("{rendered}");
        }
        KbCommands::Why(args) => {
            let kb = KnowledgeBase::open(&config.kb)?;
            catch_up_views(&kb)?;
            let events = kb.why(&args.entity)?;
            let settings = args.output.resolve();
            render_why_output(&events, &settings, &args.entity)?;
        }
        KbCommands::Migrate => {
            let kb = KnowledgeBase::open(&config.kb)?;
            kb.migrate()?;
            let materializer = Materializer::new(kb.clone());
            let mut batches = 0usize;
            while materializer.sync()? {
                batches += 1;
            }
            let watermark = materializer.current_watermark()?;
            println!(
                "knowledge base migrations applied; watermark={} ({} batch(es))",
                watermark, batches
            );
        }
    }
    Ok(())
}

async fn handle_config(cmd: ConfigCommands) -> Result<(), CliError> {
    match cmd {
        ConfigCommands::Path(args) => handle_config_path(args),
    }
}

fn handle_config_path(args: ConfigPathArgs) -> Result<(), CliError> {
    let (config, layers) = Config::load_with_layers()?;
    let settings = args.output.resolve();
    if matches!(settings.mode, OutputMode::Json) || args.all {
        return render_config_chain(&config, &layers, &settings);
    }
    if let Some(path) = active_config_path(&layers) {
        println!("{}", path.display());
    } else {
        println!("no config file found; using defaults + environment overrides");
    }
    Ok(())
}

fn render_config_chain(
    config: &Config,
    layers: &[ConfigLayer],
    settings: &output::OutputSettings,
) -> Result<(), CliError> {
    if matches!(settings.mode, OutputMode::Json) {
        let json_value = build_config_json(config, layers)?;
        print_json(&json_value)?;
        return Ok(());
    }

    let mut table = Table::new(vec![
        "order".into(),
        "source".into(),
        "status".into(),
        "details".into(),
    ]);
    for (idx, layer) in layers.iter().enumerate() {
        let (source, status, details) = describe_layer(layer);
        table.add_row(vec![
            Cell::number((idx + 1).to_string()),
            Cell::text(source),
            Cell::text(status),
            Cell::text(details),
        ]);
    }
    for note in config_override_notes(layers) {
        table.add_note(note);
    }
    print_table(&table, settings)?;
    Ok(())
}

fn build_config_json(config: &Config, layers: &[ConfigLayer]) -> Result<JsonValue, CliError> {
    let sources = layers
        .iter()
        .map(|layer| match &layer.source {
            ConfigSource::Defaults => json!({
                "kind": "defaults",
                "overrides": layer.overrides.len(),
            }),
            ConfigSource::File { path, exists } => json!({
                "kind": "file",
                "path": path,
                "exists": exists,
                "overrides": layer.overrides.len(),
            }),
            ConfigSource::Env { keys } => json!({
                "kind": "env",
                "keys": keys,
                "overrides": layer.overrides.len(),
            }),
        })
        .collect::<Vec<_>>();
    let overrides = layers
        .iter()
        .flat_map(|layer| {
            let label = layer_label(layer);
            layer.overrides.iter().map(move |entry| {
                json!({
                    "source": label,
                    "key": entry.key,
                    "previous": entry.previous.clone().unwrap_or(JsonValue::Null),
                    "new_value": entry.new_value.clone(),
                })
            })
        })
        .collect::<Vec<_>>();
    let resolved = to_value(config)?;
    Ok(json!({
        "sources": sources,
        "overrides": overrides,
        "resolved": resolved,
    }))
}

fn describe_layer(layer: &ConfigLayer) -> (String, String, String) {
    match &layer.source {
        ConfigSource::Defaults => (
            "defaults".into(),
            "baseline".into(),
            format!("overrides: {}", layer.overrides.len()),
        ),
        ConfigSource::File { path, exists } => (
            path.display().to_string(),
            if *exists {
                "loaded".into()
            } else {
                "missing".into()
            },
            format!("overrides: {}", layer.overrides.len()),
        ),
        ConfigSource::Env { keys } => (
            "environment".into(),
            format!("{} key(s)", keys.len()),
            if keys.is_empty() {
                "RUNLOOP__*".into()
            } else {
                format!("keys: {}", keys.join(", "))
            },
        ),
    }
}

fn config_override_notes(layers: &[ConfigLayer]) -> Vec<String> {
    let mut notes = Vec::new();
    for layer in layers {
        let label = layer_label(layer);
        for entry in &layer.overrides {
            let previous = entry
                .previous
                .as_ref()
                .map(display_value)
                .unwrap_or_else(|| "<unset>".into());
            let new_value = display_value(&entry.new_value);
            notes.push(format!(
                "{0}: {1} -> {2} ({3})",
                entry.key, previous, new_value, label
            ));
        }
    }
    notes
}

fn layer_label(layer: &ConfigLayer) -> String {
    match &layer.source {
        ConfigSource::Defaults => "defaults".into(),
        ConfigSource::File { path, .. } => path.display().to_string(),
        ConfigSource::Env { .. } => "environment".into(),
    }
}

fn active_config_path(layers: &[ConfigLayer]) -> Option<PathBuf> {
    layers.iter().rev().find_map(|layer| match &layer.source {
        ConfigSource::File { path, exists } if *exists => Some(path.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use runloop_core::EventId;
    use serde_json::json;

    fn event_with_payload(payload: JsonValue) -> EventRecord {
        EventRecord {
            id: EventId(1),
            ts_ms: 0,
            kind: "run.started".into(),
            actor: "agent:test".into(),
            scope: "user".into(),
            payload,
            provenance: json!({}),
        }
    }

    #[test]
    fn summarize_event_prefers_status() {
        let record = event_with_payload(json!({"status":"finished","opening_id":"opening:abc"}));
        assert_eq!(summarize_event(&record), "status=finished");
    }

    #[test]
    fn summarize_event_falls_back_to_pairs() {
        let record = event_with_payload(json!({"foo":"bar","baz":42}));
        assert_eq!(summarize_event(&record), "foo=bar, baz=42");
    }
}

fn render_query_output(
    result: &runloop_kb::QueryResult,
    settings: &output::OutputSettings,
) -> Result<(), CliError> {
    match settings.mode {
        OutputMode::Json => {
            let value = to_value(result)?;
            print_json(&value)?;
        }
        OutputMode::Table => {
            let mut table = Table::new(result.columns.clone());
            for row in &result.rows {
                let mut cells = Vec::new();
                for column in &result.columns {
                    let value = row
                        .as_object()
                        .and_then(|obj| obj.get(column))
                        .cloned()
                        .unwrap_or(JsonValue::Null);
                    if value.is_number() {
                        cells.push(Cell::number(display_value(&value)));
                    } else {
                        cells.push(Cell::text(display_value(&value)));
                    }
                }
                table.add_row(cells);
            }
            if result.rows.is_empty() {
                table.add_note("query returned 0 rows");
            }
            print_table(&table, settings)?;
        }
    }
    Ok(())
}

fn render_why_output(
    events: &[EventRecord],
    settings: &output::OutputSettings,
    entity: &str,
) -> Result<(), CliError> {
    match settings.mode {
        OutputMode::Json => {
            let value = to_value(events)?;
            print_json(&value)?;
        }
        OutputMode::Table => {
            if events.is_empty() {
                println!("no events found for {entity}");
                return Ok(());
            }
            let mut table = Table::new(vec![
                "ts_ms".into(),
                "event".into(),
                "kind".into(),
                "actor".into(),
                "scope".into(),
                "summary".into(),
            ]);
            for event in events {
                table.add_row(vec![
                    Cell::number(event.ts_ms.to_string()),
                    Cell::text(event.id.to_string()),
                    Cell::text(event.kind.clone()),
                    Cell::text(event.actor.clone()),
                    Cell::text(event.scope.clone()),
                    Cell::text(summarize_event(event)),
                ]);
            }
            print_table(&table, settings)?;
        }
    }
    Ok(())
}

fn summarize_event(event: &EventRecord) -> String {
    if let Some(status) = event.payload.get("status").and_then(|v| v.as_str()) {
        return format!("status={status}");
    }
    if let Some(opening_id) = event.payload.get("opening_id").and_then(|v| v.as_str()) {
        return format!("opening_id={opening_id}");
    }
    if let Some(obj) = event.payload.as_object() {
        let mut pairs = Vec::new();
        for (idx, (key, value)) in obj.iter().enumerate() {
            if idx >= 2 {
                break;
            }
            pairs.push(format!("{key}={}", summarize_json(value, 32)));
        }
        if !pairs.is_empty() {
            return pairs.join(", ");
        }
    }
    summarize_json(&event.payload, 48)
}

fn build_executor(config: Config) -> Result<Arc<LocalExecutor>, CliError> {
    let confirmation = Arc::new(CliConfirmationProvider::new(
        config.security.confirm_external_actions,
    ));
    let secrets = Arc::new(CliSecretResolver);
    let executor = build_local_executor(config, confirmation, secrets)?;
    Ok(executor)
}

struct CliSecretResolver;

impl SecretResolver for CliSecretResolver {
    fn resolve(&self, secret_id: &str) -> Option<String> {
        env::var(secret_id).ok()
    }
}

struct CliConfirmationProvider {
    require: bool,
}

impl CliConfirmationProvider {
    fn new(require: bool) -> Self {
        Self { require }
    }
}

#[async_trait]
impl ConfirmationProvider for CliConfirmationProvider {
    async fn confirm(&self, proposal: ActionProposal) -> AgentResult<ActionDecision> {
        if !self.require {
            return Ok(ActionDecision::approved(Some(
                "auto-approved (confirm_external_actions=false)".into(),
            )));
        }
        let summary = format!(
            "Send to {} ({} recipient(s)) for opening {}",
            proposal.recipients.join(", "),
            proposal.recipients.len(),
            proposal.opening_id
        );
        let prompt = format!("{summary}? [y/N]: ");
        let decision = task::spawn_blocking(move || -> AgentResult<bool> {
            print!("{prompt}");
            io::stdout().flush().map_err(AgentError::Io)?;
            let mut line = String::new();
            io::stdin().read_line(&mut line).map_err(AgentError::Io)?;
            let choice = line.trim().to_ascii_lowercase();
            Ok(choice == "y" || choice == "yes")
        })
        .await
        .map_err(|err| AgentError::Other(err.to_string()))??;
        if decision {
            Ok(ActionDecision::approved(Some("approved via CLI".into())))
        } else {
            Ok(ActionDecision::rejected(Some("declined via CLI".into())))
        }
    }
}

async fn submit_via_daemon(config: &Config, prepared: &PreparedOpening) -> Result<(), CliError> {
    // Resolve socket path with short-circuit rules
    let path = resolve_socket_path(config).await?;

    // Connect to the bus via IPC
    let bus = Bus::connect(&path)
        .await
        .map_err(|_| CliError::DaemonUnreachable(path.display().to_string()))?;

    // Subscribe to control responses before publishing request
    let mut ctrl_sub = bus
        .subscribe("rlp/ctrl")
        .await
        .map_err(|_| CliError::DaemonUnreachable(path.display().to_string()))?;

    // Prepare request
    let request_id = TraceId::new();
    let submit = ControlRequest::RunSubmit(RunSubmitRequest {
        request_id,
        opening_yaml: prepared._yaml.clone(),
    });
    let payload = encode_payload(CT_CTRL_REQ, &submit, None)?;

    let header = Header {
        schema_id: CT_CTRL_REQ,
        created_at_ms: current_millis(),
        ttl_ms: 30_000, // per MVP spec
        trace_id: uuid_to_u128(request_id.0),
        msg_id: next_msg_id(),
        ..Header::default()
    };
    let message = runloop_bus::Message::new(header, Bytes::from(payload))
        .map_err(|e| CliError::Core(e.into()))?;

    // Publish with a single retry on immediate failure (pre-accept)
    let publish_result = bus.publish("rlp/ctrl", message.clone()).await;
    if publish_result.is_err() {
        // Retry once with jittered backoff
        let jitter = 100 + (fastrand::u32(0..300) as u64);
        time::sleep(Duration::from_millis(jitter)).await;
        bus.publish("rlp/ctrl", message)
            .await
            .map_err(|_| CliError::DaemonUnreachable(path.display().to_string()))?;
    }

    // Await RunAccepted up to 2000 ms
    let accept = time::timeout(Duration::from_millis(2000), async move {
        while let Some(msg) = ctrl_sub.next().await {
            if msg.header.schema_id != CT_CTRL_RESP {
                continue;
            }
            // Ensure correlation on trace id
            if msg.header.trace_id != uuid_to_u128(request_id.0) {
                continue;
            }
            let decoded = decode_payload::<ControlResponse>(CT_CTRL_RESP, &msg.body)
                .map_err(|e| CliError::Core(e.into()))?;
            match decoded.payload {
                ControlResponse::RunAccepted(acc) => return Ok::<RunAccepted, CliError>(acc),
                ControlResponse::RunRejected {
                    request_id: _rid,
                    reason,
                } => {
                    return Err(CliError::RunRejected {
                        code: "UNKNOWN".into(),
                        message: reason,
                    });
                }
                ControlResponse::RunCancelled { .. } => {
                    return Err(CliError::RunRejected {
                        code: "CANCELLED".into(),
                        message: "run cancelled before start".into(),
                    });
                }
            }
        }
        Err(CliError::AcceptTimeout)
    })
    .await
    .unwrap_or(Err(CliError::AcceptTimeout))?;

    // Subscribe to unified run event stream
    let topic = format!("rlp/runs/{}/events", accept.trace_id);
    let mut ev_sub = bus
        .subscribe(&topic)
        .await
        .map_err(|_| CliError::DaemonUnreachable(path.display().to_string()))?;

    // Stream CT_RUN_EVENT frames to NDJSON, mapping to the existing format
    let mut final_success: Option<bool> = None;
    while let Some(msg) = ev_sub.next().await {
        if msg.header.schema_id != CT_RUN_EVENT {
            continue;
        }
        match decode_payload::<serde_json::Value>(CT_RUN_EVENT, &msg.body) {
            Ok(env) => {
                if let Err(err) = emit_ndjson_from_run_event_env(
                    TraceId(uuid_from_u128(msg.header.trace_id)),
                    &accept.opening_id,
                    &accept.opening_name,
                    &env.payload,
                ) {
                    eprintln!("warning: failed to render event: {err}");
                }
                let kind = env.payload.get("kind").and_then(|v| v.as_str());
                let success = env
                    .payload
                    .get("meta")
                    .and_then(|m| m.get("success"))
                    .and_then(|v| v.as_bool());
                // Terminal when daemon emits a status event with meta.success
                if matches!(kind, Some("status")) && success.is_some() {
                    final_success = success;
                    break;
                }
            }
            Err(err) => {
                eprintln!("warning: failed to decode run event: {err}");
            }
        }
    }

    match final_success {
        Some(true) => Ok(()),
        Some(false) => Err(CliError::RunFailure(accept.trace_id)),
        None => Err(CliError::RunFailure(accept.trace_id)),
    }
}

fn current_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn uuid_to_u128(id: Uuid) -> u128 {
    id.as_u128()
}

fn uuid_from_u128(v: u128) -> Uuid {
    Uuid::from_u128(v)
}

fn next_msg_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

async fn resolve_socket_path(config: &Config) -> Result<PathBuf, CliError> {
    let mut tried = Vec::new();
    // 1) Explicit runtime.socket_path: short-circuit; error if unreachable
    if let Some(path) = config
        .runtime
        .socket_path
        .as_ref()
        .filter(|p| !p.is_empty())
    {
        let p = PathBuf::from(path);
        tried.push(p.clone());
        // Probe via a simple UnixStream connect (even though Bus::connect will as well)
        match UnixStream::connect(&p).await {
            Ok(_) => return Ok(p),
            Err(_) => return Err(CliError::DaemonUnreachable(p.display().to_string())),
        }
    }

    // 2) ${runtime.sockets_dir}/rmp.sock (string field, check non-empty)
    if !config.runtime.sockets_dir.trim().is_empty() {
        let p = PathBuf::from(&config.runtime.sockets_dir).join("rmp.sock");
        tried.push(p.clone());
        if UnixStream::connect(&p).await.is_ok() {
            return Ok(p);
        }
    }

    // 3) ~/.runloop/sock/rmp.sock
    if let Some(home) = home_dir() {
        let p = home.join(".runloop").join("sock").join("rmp.sock");
        tried.push(p.clone());
        if UnixStream::connect(&p).await.is_ok() {
            return Ok(p);
        }
    }

    // 4) /run/runloop/rmp.sock
    let p = PathBuf::from("/run/runloop/rmp.sock");
    tried.push(p.clone());
    if UnixStream::connect(&p).await.is_ok() {
        return Ok(p);
    }

    Err(CliError::DaemonUnavailable(format_paths(&tried)))
}

fn format_paths(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "<none>".into();
    }
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

struct RunEventRecorder {
    kb: KnowledgeBase,
}

impl RunEventRecorder {
    fn new(config: &Config) -> Result<Self, CliError> {
        let kb = KnowledgeBase::open(&config.kb)?;
        kb.migrate()?;
        Ok(Self { kb })
    }

    fn started(&self, trace_id: &TraceId, opening_id: &OpeningId) -> Result<(), CliError> {
        self.record("run.started", trace_id, opening_id, "started")
    }

    fn finished(
        &self,
        trace_id: &TraceId,
        opening_id: &OpeningId,
        status: &str,
    ) -> Result<(), CliError> {
        self.record("run.finished", trace_id, opening_id, status)
    }

    fn record(
        &self,
        kind: &str,
        trace_id: &TraceId,
        opening_id: &OpeningId,
        status: &str,
    ) -> Result<(), CliError> {
        let payload = json!({
            "opening_id": opening_id.to_string(),
            "status": status,
        });
        let provenance = Provenance {
            trace_id: trace_id.to_string(),
            opening_id: opening_id.to_string(),
            agent_id: CLI_AGENT_ID.into(),
            inputs_hash: None,
            rationale: None,
        };
        self.kb.propose(StateDelta::new(
            kind,
            CLI_AGENT_ID,
            Some("user".into()),
            payload,
            provenance,
        ))?;
        Ok(())
    }

    fn flush(&self) -> Result<(), CliError> {
        let materializer = Materializer::new(self.kb.clone());
        while materializer.sync()? {}
        Ok(())
    }
}

struct PreparedOpening {
    opening: Opening,
    _yaml: String,
    params: JsonValue,
    opening_name: String,
}

fn prepare_opening(path: &str, params_override: Option<&str>) -> Result<PreparedOpening, CliError> {
    let source = fs::read_to_string(path)?;
    let mut doc: YamlValue = serde_yaml::from_str(&source)
        .map_err(|err| CliError::InvalidTrace(format!("invalid YAML: {err}")))?;

    if let Some(params_json) = params_override {
        let parsed: JsonValue = serde_json::from_str(params_json)?;
        let obj = parsed
            .as_object()
            .ok_or_else(|| CliError::InvalidParams("params must be a JSON object".into()))?;

        let mapping = doc
            .as_mapping_mut()
            .ok_or_else(|| CliError::InvalidTrace("opening YAML must be a mapping".into()))?;

        let params_entry = mapping
            .entry(YamlValue::from("params"))
            .or_insert_with(|| YamlValue::Mapping(YamlMapping::new()));

        let params_mapping = params_entry
            .as_mapping_mut()
            .ok_or_else(|| CliError::InvalidParams("existing params must be a mapping".into()))?;

        for (key, value) in obj {
            let yaml_value = serde_yaml::to_value(value.clone())
                .map_err(|err| CliError::InvalidParams(format!("invalid param value: {err}")))?;
            params_mapping.insert(YamlValue::from(key.clone()), yaml_value);
        }
    }

    let opening_yaml =
        serde_yaml::to_string(&doc).map_err(|err| CliError::InvalidTrace(err.to_string()))?;
    let opening = parse_opening_str(&opening_yaml)?;
    let params = JsonValue::Object(opening.params.clone());
    let opening_name = opening.name.clone();
    Ok(PreparedOpening {
        opening,
        _yaml: opening_yaml,
        params,
        opening_name,
    })
}

async fn run_opening_locally(
    config: &Config,
    prepared: PreparedOpening,
    trace_out: Option<PathBuf>,
) -> Result<(), CliError> {
    let PreparedOpening {
        opening,
        opening_name,
        params,
        ..
    } = prepared;
    let executor = build_executor(config.clone())?;
    let recorder = RunEventRecorder::new(config)?;
    let runner = Runner::new(opening, executor);
    let mut emitter =
        RunEventEmitter::new(runner.trace_id(), runner.opening_id(), opening_name, params);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let runner = runner.with_event_tx(tx);
    let trace_id = runner.trace_id();
    let opening_id = runner.opening_id();

    recorder.started(&trace_id, &opening_id)?;
    emitter.emit_run_started()?;

    let mut run_future = Box::pin(async move { runner.run().await });
    let mut receiver_open = true;

    let result: Result<RunReport, RunnerError> = loop {
        tokio::select! {
            res = &mut run_future => break res,
            maybe_event = rx.recv(), if receiver_open => {
                match maybe_event {
                    Some(event) => emitter.handle_runner_event(event)?,
                    None => receiver_open = false,
                }
            }
        }
    };

    drop(run_future);

    while let Some(event) = rx.recv().await {
        emitter.handle_runner_event(event)?;
    }

    match result {
        Ok(report) => {
            let node_summaries = emitter.emit_node_finishes(&report.trace)?;
            let run_status = if report.trace.success { "ok" } else { "error" };
            emitter.emit_run_finished(run_status, node_summaries)?;
            if let Some(path) = trace_out {
                let file = File::create(&path)?;
                to_writer_pretty(file, &report.trace)?;
            }
            recorder.finished(
                &report.trace.trace_id,
                &report.trace.opening_id,
                if report.trace.success {
                    "finished"
                } else {
                    "failed"
                },
            )?;
            recorder.flush()?;
            if report.trace.success {
                Ok(())
            } else {
                Err(CliError::RunFailure(report.trace.trace_id))
            }
        }
        Err(err) => {
            let summaries = emitter.summarize_failure();
            emitter.emit_run_finished("error", summaries)?;
            recorder.finished(&trace_id, &opening_id, "failed")?;
            recorder.flush()?;
            Err(CliError::Runner(err))
        }
    }
}
