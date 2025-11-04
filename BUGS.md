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

## 4. Bus Sends Panic Inside Nested Tokio Runtimes

- **Status:** High  
- **Scope:** `crates/runtime/src/runtime.rs:493-505`

### What Happens
When `Runtime::send` runs inside an existing Tokio executor, the call to `spawner.block_on` invokes `TokioHandle::block_on`, which panics with "cannot start a runtime from within a runtime." Any caller that embeds the runtime inside async services hits this panic the first time it forwards a bus message.

### Impact
- Crashes supervising services that dispatch messages from async contexts.
- Prevents integrating the runtime into async control planes without wrapping every `send` in a separate thread.
- Masks the underlying bus error semantics because the panic aborts execution paths.

### Root Cause
The new bus integration replaced the non-async `inbox.try_send` path with a hard `block_on` of the async bus send operation, ignoring Tokio's `!Send`-from-runtime constraints.

### Recommended Fix
Offload the async send without calling `block_on` on the current handle—e.g., spawn a task that writes to the bus and synchronizes via a oneshot, or fall back to the in-process channel when the runtime already has direct access to the agent inbox.

## 5. `model_complete` Corrupts Prompt Buffer When Reserving Output Space

- **Status:** High  
- **Scope:** `crates/runtime/src/hostcalls.rs:314-345`

### What Happens
The hostcall treats `prompt_len` as both the size of the prompt to read and the available capacity for writing the model output. Guests that pass a buffer larger than the actual prompt (to leave room for the response) have their prompt string read with trailing zeroed padding, which the echo broker then mirrors back.

### Impact
- Model prompts delivered to the broker include garbage bytes, causing nondeterministic completions.
- There is no way for guests to request responses longer than the prompt without contaminating their request payload.
- Future, non-echo providers will misinterpret prompts and return incorrect or rejected completions.

### Root Cause
`read_utf8` consumes `prompt_len` bytes before the host knows how much of that buffer contains initialized prompt data. The API conflates "prompt length" with "output capacity."

### Recommended Fix
Extend the hostcall signature to separate the prompt input length from the output buffer capacity (or write into a distinct pointer). The host should read only the initialized prompt slice and use an explicit capacity when checking for response truncation.
