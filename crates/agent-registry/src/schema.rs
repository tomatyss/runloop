use jsonschema::{Validator, error::ValidationError, error::ValidationErrorKind};
use serde_json::Value as JsonValue;
use thiserror::Error;

/// Detailed violation emitted by JSON Schema validation.
#[derive(Clone, Debug)]
pub struct SchemaViolation {
    pub instance_path: String,
    pub schema_path: String,
    pub message: String,
    pub kind: String,
    pub value: JsonValue,
}

/// Error returned when compiling or applying a schema fails.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SchemaValidationError {
    #[error("invalid schema: {0}")]
    InvalidSchema(String),
    #[error("validation failed")]
    Violations(Vec<SchemaViolation>),
}

/// Validate a JSON value against the provided schema, returning structured violations.
pub fn validate_instance(
    schema: &JsonValue,
    instance: &JsonValue,
) -> Result<(), SchemaValidationError> {
    let validator = Validator::new(schema)
        .map_err(|err| SchemaValidationError::InvalidSchema(err.to_string()))?;
    let mut issues = Vec::new();
    for error in validator.iter_errors(instance) {
        issues.push(schema_violation(error));
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(SchemaValidationError::Violations(issues))
    }
}

fn schema_violation(err: ValidationError<'_>) -> SchemaViolation {
    SchemaViolation {
        instance_path: err.instance_path().as_str().to_string(),
        schema_path: err.schema_path().as_str().to_string(),
        message: err.to_string(),
        kind: describe_kind(err.kind()),
        value: err.instance().clone().into_owned(),
    }
}

fn describe_kind(kind: &ValidationErrorKind) -> String {
    match kind {
        ValidationErrorKind::Type { .. } => "type".into(),
        ValidationErrorKind::Required { .. } => "required".into(),
        ValidationErrorKind::MinLength { .. } => "minLength".into(),
        ValidationErrorKind::MaxLength { .. } => "maxLength".into(),
        ValidationErrorKind::Minimum { .. } => "minimum".into(),
        ValidationErrorKind::Maximum { .. } => "maximum".into(),
        ValidationErrorKind::Enum { .. } => "enum".into(),
        ValidationErrorKind::Pattern { .. } => "pattern".into(),
        ValidationErrorKind::AnyOf { .. } => "anyOf".into(),
        ValidationErrorKind::OneOfNotValid { .. }
        | ValidationErrorKind::OneOfMultipleValid { .. } => "oneOf".into(),
        ValidationErrorKind::Format { .. } => "format".into(),
        ValidationErrorKind::AdditionalProperties { .. } => "additionalProperties".into(),
        ValidationErrorKind::AdditionalItems { .. } => "additionalItems".into(),
        ValidationErrorKind::UniqueItems => "uniqueItems".into(),
        ValidationErrorKind::Contains => "contains".into(),
        ValidationErrorKind::UnevaluatedProperties { .. } => "unevaluatedProperties".into(),
        ValidationErrorKind::UnevaluatedItems { .. } => "unevaluatedItems".into(),
        ValidationErrorKind::ExclusiveMinimum { .. } => "exclusiveMinimum".into(),
        ValidationErrorKind::ExclusiveMaximum { .. } => "exclusiveMaximum".into(),
        ValidationErrorKind::MultipleOf { .. } => "multipleOf".into(),
        ValidationErrorKind::MinItems { .. } => "minItems".into(),
        ValidationErrorKind::MaxItems { .. } => "maxItems".into(),
        ValidationErrorKind::MinProperties { .. } => "minProperties".into(),
        ValidationErrorKind::MaxProperties { .. } => "maxProperties".into(),
        ValidationErrorKind::Not { .. } => "not".into(),
        ValidationErrorKind::FalseSchema => "falseSchema".into(),
        ValidationErrorKind::Referencing(_) => "$ref".into(),
        ValidationErrorKind::Custom { .. } => "custom".into(),
        _ => format!("{kind:?}"),
    }
}
