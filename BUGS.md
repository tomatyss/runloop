# Runtime Bug Backlog

The new `runloop-runtime` crate introduces a richer Wasmtime embedding, but several high-priority defects remain outstanding. Each issue below captures the observed behavior, impact, suspected root cause, and recommended next actions.

## 1. Spawn Success Is Reported Before Guest Startup Completes

- **Status:** Critical  
- **Scope:** `crates/runtime/src/runtime.rs:118-224`

### What Happens
`Runtime::spawn` returns `Ok(AgentHandle)` immediately after the worker thread is spawned. If Wasmtime instantiation fails or `_start` traps, the guest thread exits with an `Err`, but the main thread only sees that failure when `kill` joins the handle (or never, if kill is not called). Callers therefore observe a "running" agent even though startup failed.

### Impact
- Transient Wasm bugs look like hung agents instead of failing fast.
- Supervisors may enqueue messages to an agent that never booted, leading to lost work.
- Observability and audit trails lack the failure signal until teardown.

### Root Cause
`spawn` does not block on any readiness signal from the worker thread. The join handle is stored, but the function returns before `_start` finishes.

### Recommended Fix
Introduce a `oneshot` channel (or similar) so the worker thread reports success or failure before `spawn` returns. Propagate the first error immediately and only publish the agent handle once the guest is truly running.

## 2. Stdout/Stderr Buffers Grow Without Bound

- **Status:** High  
- **Scope:** `crates/runtime/src/runtime.rs:90-114,334-340`

### What Happens
`RingWriter::write` pushes every byte into both the bounded `OutputRing` and an auxiliary `Vec<u8>` protected by `RwLock`. These vectors are never truncated or exposed, so they grow for the lifetime of the agent.

### Impact
- A noisy or malicious guest can consume unbounded host memory.
- Long-lived agents leak memory proportional to their log volume.

### Root Cause
The `_stdout_buffer` and `_stderr_buffer` fields exist but no consumer drains them. They accumulate log data indefinitely.

### Recommended Fix
Either remove the unbounded buffers entirely, or enforce a capacity (matching the ring) and drop old data when full.

## 3. Module Cache Ignores On-Disk Updates

- **Status:** Medium  
- **Scope:** `crates/runtime/src/module_cache.rs:23-31`

### What Happens
`ModuleCache::load` stores every compiled `Module` in a `DashMap<PathBuf, Arc<Module>>` and never invalidates entries. Once cached, future calls return the old module even if the Wasm binary has been modified on disk.

### Impact
- Operators cannot deploy new Wasm builds without restarting the entire runtime.
- Hot-reload workflows silently run stale code, risking user-facing regressions.

### Root Cause
The cache key is the raw path, and there is no mtime/hash check to detect changes.

### Recommended Fix
Track file metadata (e.g., mtime + length) or a content hash alongside the cached module. If the file changes, evict and recompile before returning the module.

