use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;

#[derive(Debug, Clone)]
pub struct ModelBroker<P: Provider> {
    provider: Arc<P>,
}

impl<P: Provider> ModelBroker<P> {
    pub fn new(provider: P) -> Self {
        Self {
            provider: Arc::new(provider),
        }
    }

    pub fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        enforce_streaming_guard(request.stream)?;
        self.provider.complete(request)
    }
}

#[cfg(not(feature = "streaming"))]
fn enforce_streaming_guard(stream: bool) -> Result<(), ModelError> {
    if stream {
        Err(ModelError::StreamingUnsupported(
            "streaming is disabled in MVP; enable the `streaming` feature to experiment".into(),
        ))
    } else {
        Ok(())
    }
}

#[cfg(feature = "streaming")]
fn enforce_streaming_guard(_: bool) -> Result<(), ModelError> {
    Ok(())
}

pub trait Provider: Send + Sync + 'static {
    fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelRequest {
    pub prompt: String,
    pub model: String,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelResponse {
    pub output_text: String,
    pub finish_reason: FinishReason,
    pub tokens_used: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCall,
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("streaming unsupported: {0}")]
    StreamingUnsupported(String),
    #[error("provider error: {0}")]
    Provider(String),
}

#[derive(Debug, Default)]
pub struct StubProvider;

impl Provider for StubProvider {
    fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        debug!(model = %request.model, "stub provider invoked");
        Ok(ModelResponse {
            output_text: format!("[stub:{}] {}", request.model, request.prompt),
            finish_reason: FinishReason::Stop,
            tokens_used: (request.prompt.len() / 4 + 1) as u32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_guard_blocks() {
        let broker = ModelBroker::new(StubProvider::default());
        let req = ModelRequest {
            prompt: "hello".into(),
            model: "gpt-stub".into(),
            stream: true,
            max_tokens: None,
            temperature: None,
        };
        let err = broker.complete(req).unwrap_err();
        assert!(matches!(err, ModelError::StreamingUnsupported(_)));
    }

    #[test]
    fn stub_provider_returns_text() {
        let broker = ModelBroker::new(StubProvider::default());
        let req = ModelRequest {
            prompt: "hello".into(),
            model: "gpt-stub".into(),
            stream: false,
            max_tokens: None,
            temperature: Some(0.1),
        };
        let res = broker.complete(req).expect("stub works");
        assert!(res.output_text.contains("hello"));
    }
}
