//! Agent SDK utilities (hostcall wrappers, message helpers).

#![allow(dead_code)]

pub struct Client;

impl Client {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}
