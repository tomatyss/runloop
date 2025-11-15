use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use dashmap::DashMap;
use wasmtime::{Engine, Module};

use crate::error::Error;

/// Simple filesystem-backed module cache for Wasm binaries.
#[derive(Debug, Default)]
pub struct ModuleCache {
    modules: DashMap<PathBuf, CachedModule>,
}

#[derive(Debug, Clone)]
struct CachedModule {
    module: Arc<Module>,
    signature: FileSignature,
}

impl CachedModule {
    fn is_fresh(&self, signature: &FileSignature) -> bool {
        &self.signature == signature
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileSignature {
    len: u64,
    modified: Option<SystemTime>,
    file_id: Option<FileId>,
}

impl FileSignature {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            file_id: file_identifier(metadata),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileId {
    dev: u64,
    ino: u64,
    ctime: Option<(i64, i64)>,
}

impl ModuleCache {
    pub fn new() -> Self {
        Self {
            modules: DashMap::new(),
        }
    }

    pub fn load(&self, engine: &Engine, path: &Path) -> Result<Arc<Module>, Error> {
        if let Some(entry) = self.modules.get(path) {
            match fs::metadata(path) {
                Ok(metadata) => {
                    let signature = FileSignature::from_metadata(&metadata);
                    if entry.value().is_fresh(&signature) {
                        return Ok(entry.value().module.clone());
                    }
                    drop(entry);
                    return self.compile_and_store(engine, path, signature);
                }
                Err(_) => {
                    return Ok(entry.value().module.clone());
                }
            }
        }

        let metadata = fs::metadata(path)?;
        let signature = FileSignature::from_metadata(&metadata);
        self.compile_and_store(engine, path, signature)
    }

    fn compile_and_store(
        &self,
        engine: &Engine,
        path: &Path,
        signature: FileSignature,
    ) -> Result<Arc<Module>, Error> {
        let bytes = fs::read(path)?;
        let module = Module::from_binary(engine, &bytes)?;
        let module = Arc::new(module);
        self.modules.insert(
            path.to_path_buf(),
            CachedModule {
                module: module.clone(),
                signature,
            },
        );
        Ok(module)
    }
}

#[cfg(unix)]
fn file_identifier(metadata: &fs::Metadata) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;

    Some(FileId {
        dev: metadata.dev(),
        ino: metadata.ino(),
        ctime: Some((metadata.ctime(), metadata.ctime_nsec() as i64)),
    })
}

#[cfg(not(unix))]
fn file_identifier(_: &fs::Metadata) -> Option<FileId> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::thread;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;
    use wat::parse_str;

    fn write_wasm(path: &Path, module_src: &str) {
        let bytes = parse_str(module_src).expect("valid wat");
        fs::write(path, bytes).expect("write wasm");
    }

    #[test]
    fn reloads_module_when_file_changes() {
        let dir = tempdir().unwrap();
        let wasm_path = dir.path().join("module.wasm");

        write_wasm(&wasm_path, "(module (func (export \"run\") (nop)))");

        let engine = Engine::default();
        let cache = ModuleCache::new();

        let first = cache.load(&engine, &wasm_path).unwrap();
        let initial_modified = fs::metadata(&wasm_path)
            .ok()
            .and_then(|m| m.modified().ok());

        write_wasm(
            &wasm_path,
            "(module (func (export \"run\") (i32.const 1) drop))",
        );
        wait_for_modified_change(&wasm_path, initial_modified);

        let second = cache.load(&engine, &wasm_path).unwrap();
        assert!(
            !Arc::ptr_eq(&first, &second),
            "cache should reload updated module"
        );

        let third = cache.load(&engine, &wasm_path).unwrap();
        assert!(
            Arc::ptr_eq(&second, &third),
            "subsequent loads reuse cached module"
        );
    }

    fn wait_for_modified_change(path: &Path, previous: Option<SystemTime>) {
        // Poll for a new timestamp so the test does not depend on coarse FS resolution.
        if previous.is_none() {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if Instant::now() >= deadline {
                panic!("file modification time failed to advance");
            }
            thread::sleep(Duration::from_millis(50));
            let current = fs::metadata(path)
                .ok()
                .and_then(|metadata| metadata.modified().ok());
            if current != previous {
                return;
            }
        }
    }
}
