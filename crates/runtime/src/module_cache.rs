use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use wasmtime::{Engine, Module};

use crate::error::Error;

/// Simple filesystem-backed module cache for Wasm binaries.
#[derive(Debug, Default)]
pub struct ModuleCache {
    modules: DashMap<PathBuf, Arc<Module>>,
}

impl ModuleCache {
    pub fn new() -> Self {
        Self {
            modules: DashMap::new(),
        }
    }

    pub fn load(&self, engine: &Engine, path: &Path) -> Result<Arc<Module>, Error> {
        if let Some(entry) = self.modules.get(path) {
            return Ok(entry.clone());
        }
        let bytes = fs::read(path)?;
        let module = Module::from_binary(engine, &bytes)?;
        let module = Arc::new(module);
        self.modules.insert(path.to_path_buf(), module.clone());
        Ok(module)
    }
}
