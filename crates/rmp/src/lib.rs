//! Runloop Message Protocol helpers (header + encoding).

#![allow(dead_code)]

pub mod header;
pub mod registry;

pub use header::{FrameDecodeError, HEADER_LEN, HEADER_VERSION, Header};
