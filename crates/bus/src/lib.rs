//! Local message bus abstraction (Unix domain sockets + RMP framing).

#![allow(dead_code)]

pub struct Bus;

impl Bus {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}
