use std::io;

use thiserror::Error;

/// Core error variants shared across Runloop components.
#[derive(Debug, Error)]
pub enum Error {
    /// Wrapper for I/O failures.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// Configuration parsing or validation failure.
    #[error("configuration error: {0}")]
    Config(String),
    /// Message bus related failure.
    #[error("bus error: {0}")]
    Bus(String),
    /// Runloop Message Protocol framing/decoding failure.
    #[error("rmp error: {0}")]
    Rmp(String),
    /// Runtime (Wasmtime/container) failure.
    #[error("runtime error: {0}")]
    Runtime(String),
    /// Knowledge base failure.
    #[error("kb error: {0}")]
    Kb(String),
    /// Model broker failure.
    #[error("broker error: {0}")]
    Broker(String),
    /// Router failure.
    #[error("router error: {0}")]
    Router(String),
    /// Opening engine failure.
    #[error("opening error: {0}")]
    Opening(String),
    /// Capability denied enforcement.
    #[error("capability denied: {0}")]
    CapDenied(String),
    /// Operation exceeded timeout.
    #[error("timeout exceeded: {0}")]
    Timeout(String),
    /// Operation exceeded configured budget.
    #[error("budget exceeded: {0}")]
    BudgetExceeded(String),
}
