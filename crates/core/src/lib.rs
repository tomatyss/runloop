//! Core types shared across Runloop crates.

pub mod config;
pub mod error;
pub mod ids;

pub use config::Config;
pub use error::Error;
pub use ids::{AgentId, EventId, OpeningId, TraceId};
