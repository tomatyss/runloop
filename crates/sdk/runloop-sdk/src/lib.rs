//! Agent-facing SDK and shim helpers used by native (non-WASM) bundles.

#![deny(missing_docs)]

pub mod caps;
mod error;
pub mod handshake;
pub mod shim;

pub use caps::{EffectiveCaps, FsAccess, NamespaceCaps, NetAccess};
pub use error::{Error, Result};
pub use handshake::{AgentHello, AgentHelloAck, CapabilitySummary, TraceContext};
pub use shim::{PublishMeta, ShimClient, ShimConfig};
