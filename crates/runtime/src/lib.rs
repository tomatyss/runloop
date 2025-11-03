//! Agent runtime integration (Wasmtime, capability enforcement, shims).

#![allow(dead_code)]

pub struct Runtime;

impl Runtime {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}
