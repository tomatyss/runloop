//! Model broker facade for completion providers (MVP: echo provider).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct Broker;

impl Broker {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Execute a completion request. MVP echoes the prompt for determinism.
    pub fn complete(&self, request: &CompletionRequest) -> CompletionResponse {
        CompletionResponse {
            model_used: request
                .model
                .clone()
                .unwrap_or_else(|| "local:echo".to_string()),
            output: format!("echo: {}", request.prompt),
        }
    }
}

impl Default for Broker {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub prompt: String,
    pub model: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub model_used: String,
    pub output: String,
}
