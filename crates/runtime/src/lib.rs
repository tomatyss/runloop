//! Runloop agent runtime – Wasmtime embedding, capability enforcement, and lifecycle utilities.

#![deny(unsafe_code)]

mod audit;
mod caps;
mod error;
mod module_cache;
mod output;
mod policy;
mod runtime;
mod spec;
mod stats;
mod wasi_dir;

pub use caps::{CapabilitySet, Caps, FsCapability, NetLocation};
pub use error::Error;
pub use runtime::{AgentHandle, Runtime};
pub use spec::{AgentIdentity, AgentSpec};
pub use stats::AgentStats;
