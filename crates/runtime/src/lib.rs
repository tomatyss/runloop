//! WASM runtime scaffolding placeholder.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("engine not initialised")]
    NotInitialised,
}

pub struct Engine {
    initialised: bool,
}

impl Engine {
    pub fn new() -> Self {
        Self { initialised: false }
    }

    pub fn initialise(&mut self) {
        self.initialised = true;
    }

    pub fn is_ready(&self) -> bool {
        self.initialised
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_initialises() {
        let mut engine = Engine::new();
        assert!(!engine.is_ready());
        engine.initialise();
        assert!(engine.is_ready());
    }
}
