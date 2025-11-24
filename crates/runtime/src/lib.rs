//! Runloop agent runtime – Wasmtime embedding, capability enforcement, and lifecycle utilities.

#![deny(unsafe_code)]

mod audit;
mod caps;
mod error;
mod hostcalls;
mod module_cache;
mod output;
mod policy;
mod ready;
mod runtime;
mod secrets;
mod spec;
mod stats;

pub use caps::{CapabilitySet, Caps, DebugPreopen, FsCapability, NetLocation};
pub use error::Error;
pub use hostcalls::HostcallStats;
pub use runtime::{AgentHandle, AgentMetricSample, AuditPolicy, Runtime, RuntimeBuilder};
pub use secrets::{SecretProvider, SecretStore};
pub use spec::{AgentIdentity, AgentSpec};
pub use stats::AgentStats;
