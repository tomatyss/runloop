#![allow(unsafe_code)]
//! Lightweight hostcall helpers for wasm agents.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const MODEL_ESTREAM: i32 = -1;
const MODEL_EBUDGET: i32 = -2;
const MODEL_ETIMEOUT: i32 = -3;
const MODEL_EPROVIDER: i32 = -4;
const MODEL_EINVAL: i32 = -5;
const MODEL_ENOSPACE: i32 = -6;
const MODEL_ECANCELLED: i32 = -7;

const EXEC_EINVAL: i32 = -1;
const EXEC_ESPAWN: i32 = -2;
const EXEC_ESIGNAL: i32 = -3;
const EXEC_ENOSPACE: i32 = -4;

#[allow(improper_ctypes)]
#[link(wasm_import_module = "runloop")]
unsafe extern "C" {
    fn model_complete(
        req_ptr: i32,
        req_len: i32,
        out_ptr: i32,
        out_cap: i32,
        meta_ptr: i32,
        meta_cap: i32,
    ) -> i32;

    fn exec_spawn_capture(
        cmd_ptr: i32,
        cmd_len: i32,
        stdout_ptr: i32,
        stdout_cap: i32,
        stderr_ptr: i32,
        stderr_cap: i32,
    ) -> i32;
}

/// Identifier for a distributed trace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct TraceId(pub Uuid);

/// Optional model parameters forwarded to providers.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ModelParams {
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stop: Option<Vec<String>>,
}

/// Execution request for the broker.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ModelRequest {
    #[serde(default)]
    pub trace_id: TraceId,
    pub model: String,
    pub prompt: String,
    #[serde(default)]
    pub role_system: Option<String>,
    #[serde(default)]
    pub params: Option<ModelParams>,
    #[serde(default)]
    pub budget_tokens: Option<u32>,
    #[serde(default)]
    pub timeout_ms: Option<u32>,
    #[serde(default)]
    pub cache_ttl_ms: Option<u32>,
    #[serde(default)]
    pub cache_key: Option<String>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub extras: Option<serde_json::Value>,
}

/// Metadata returned alongside the model output text.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ModelOutputMeta {
    pub tokens_in: Option<u32>,
    pub tokens_out: Option<u32>,
    pub cached: bool,
    pub provider: String,
    pub provider_model: String,
    pub latency_ms: u32,
    pub finish_reason: Option<String>,
}

/// Normalised broker response with text and metadata.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ModelOutput {
    pub text: String,
    pub meta: Option<ModelOutputMeta>,
}

/// Errors surfaced by the host model adapter.
#[derive(Error, Debug)]
pub enum ModelError {
    #[error("streaming unsupported")]
    StreamUnsupported,
    #[error("budget exceeded")]
    BudgetExceeded,
    #[error("request timed out")]
    Timeout,
    #[error("provider failed")]
    ProviderFault,
    #[error("invalid request")]
    InvalidRequest,
    #[error("output truncated")]
    NoSpace,
    #[error("request cancelled")]
    Cancelled,
    #[error("failed to serialize request: {0}")]
    Serialize(String),
    #[error("invalid utf8 output: {0}")]
    Utf8(String),
    #[error("failed to decode metadata: {0}")]
    Meta(String),
    #[error("hostcall failed: {0}")]
    Host(String),
}

/// Errors surfaced by the exec capture hostcall.
#[derive(Error, Debug)]
pub enum ExecError {
    #[error("invalid arguments")]
    Invalid,
    #[error("spawn failed")]
    SpawnFailed,
    #[error("process terminated by signal")]
    Signal,
    #[error("insufficient buffer for output")]
    NoSpace,
    #[error("invalid utf8 output: {0}")]
    Utf8(String),
    #[error("hostcall failed: {0}")]
    Host(String),
}

/// Result of an exec capture hostcall.
#[derive(Clone, Debug)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Invoke the host model broker and decode text + metadata.
pub fn complete_model(
    request: &ModelRequest,
    out_cap: usize,
    meta_cap: usize,
) -> Result<ModelOutput, ModelError> {
    let req_bytes =
        rmp_serde::to_vec(request).map_err(|err| ModelError::Serialize(err.to_string()))?;
    let mut out = vec![0u8; out_cap];
    let mut meta = vec![0u8; meta_cap];
    let status = unsafe {
        model_complete(
            req_bytes.as_ptr() as i32,
            req_bytes.len() as i32,
            out.as_mut_ptr() as i32,
            out.len() as i32,
            meta.as_mut_ptr() as i32,
            meta.len() as i32,
        )
    };

    if status < 0 {
        return Err(map_model_error(status));
    }
    let written = status as usize;
    let text = String::from_utf8(out[..written].to_vec())
        .map_err(|err| ModelError::Utf8(err.to_string()))?;

    let meta = if meta_cap >= 4 {
        let len = u32::from_le_bytes(meta[0..4].try_into().unwrap_or_default()) as usize;
        if len > 0 && len + 4 <= meta.len() {
            let payload = &meta[4..4 + len];
            Some(
                rmp_serde::from_slice(payload)
                    .map_err(|err| ModelError::Meta(err.to_string()))?,
            )
        } else {
            None
        }
    } else {
        None
    };

    Ok(ModelOutput { text, meta })
}

/// Execute a command and capture stdout/stderr (length-prefixed) from the host.
pub fn exec_capture(
    command: &str,
    stdout_cap: usize,
    stderr_cap: usize,
) -> Result<ExecResult, ExecError> {
    let mut stdout = vec![0u8; stdout_cap];
    let mut stderr = vec![0u8; stderr_cap];
    let status = unsafe {
        exec_spawn_capture(
            command.as_ptr() as i32,
            command.len() as i32,
            stdout.as_mut_ptr() as i32,
            stdout.len() as i32,
            stderr.as_mut_ptr() as i32,
            stderr.len() as i32,
        )
    };
    if status == EXEC_EINVAL {
        return Err(ExecError::Invalid);
    }
    if status == EXEC_ESPAWN {
        return Err(ExecError::SpawnFailed);
    }
    if status == EXEC_ESIGNAL {
        return Err(ExecError::Signal);
    }
    if status == EXEC_ENOSPACE {
        return Err(ExecError::NoSpace);
    }
    if status < 0 {
        return Err(ExecError::Host(format!("unexpected code {status}")));
    }

    let stdout = decode_prefixed(&stdout)
        .map_err(|err| ExecError::Utf8(err.to_string()))?
        .unwrap_or_default();
    let stderr = decode_prefixed(&stderr)
        .map_err(|err| ExecError::Utf8(err.to_string()))?
        .unwrap_or_default();

    Ok(ExecResult {
        exit_code: status,
        stdout,
        stderr,
    })
}

fn decode_prefixed(buf: &[u8]) -> Result<Option<String>, std::string::FromUtf8Error> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_le_bytes(buf[0..4].try_into().unwrap_or_default()) as usize;
    if len == 0 || len + 4 > buf.len() {
        return Ok(None);
    }
    String::from_utf8(buf[4..4 + len].to_vec()).map(Some)
}

fn map_model_error(code: i32) -> ModelError {
    match code {
        MODEL_ESTREAM => ModelError::StreamUnsupported,
        MODEL_EBUDGET => ModelError::BudgetExceeded,
        MODEL_ETIMEOUT => ModelError::Timeout,
        MODEL_EPROVIDER => ModelError::ProviderFault,
        MODEL_EINVAL => ModelError::InvalidRequest,
        MODEL_ENOSPACE => ModelError::NoSpace,
        MODEL_ECANCELLED => ModelError::Cancelled,
        other => ModelError::Host(format!("unexpected code {other}")),
    }
}
