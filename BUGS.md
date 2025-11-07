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

## 6. Predicate Comparisons Overflow On Large Unsigned Values

- **Status:** High  
- **Scope:** `crates/openings/src/runner.rs:216-254`

### What Happens

`evaluate_predicate` handles integer literals by first checking `num.as_i64()`, then falling back to `num.as_u64()` and casting the result to `i64`. Values above `i64::MAX` therefore wrap into the negative range before being compared, so predicates like `foo.score >= 9223372036854775808` evaluate against a negative number.

### Impact

- Openings that gate on monotonically increasing counters or IDs (e.g., Snowflake IDs) report false negatives.
- Success conditions tied to large numeric thresholds can be bypassed because the comparison sees wrapped values.
- Replay traces drift from real world runs when large unsigned outputs appear.

### Root Cause

The integer code path truncates `u64` values to `i64` instead of keeping the comparison in an unsigned domain (or promoting both sides to `f64`/BigInt).

### Recommended Fix

Preserve unsigned comparisons—either compare using `u128`/`u64` throughout or branch on the literal type so `Literal::Integer` drives `as_i128`/`as_u128`-aware logic without lossy casts.

## 7. Default Config Emits Spurious Missing-Directory Warnings

- **Status:** Medium  
- **Scope:** `crates/core/src/config.rs:48-134`

### What Happens

`Config::validate` now calls `warn_missing_search_dirs` for openings and agents. The new defaults include repo-relative paths (`./examples/openings`, `./agents`) and user directories (`~/.runloop/*`) that typically do not exist on a fresh install, so every CLI invocation prints multiple WARN lines.

### Impact

- Users see noisy warnings even when nothing is misconfigured, reducing trust in diagnostics.
- CI and scripted runs that treat WARN as noteworthy now look degraded after this change.

### Root Cause

The helper unconditionally warns on missing directories, including those injected by the default configuration instead of user-provided overrides.

### Recommended Fix

Downgrade the log level (e.g., to debug) for default paths, or emit warnings only for directories supplied via config files/env overrides. Alternatively, create the repo-relative directories during validation instead of warning.

## 8. Node Status Output Order Is Non-Deterministic

- **Status:** Low  
- **Scope:** `crates/openings/src/runner.rs:170-212`

### What Happens

`RunReport` collects `node_records` from a `HashMap`, and the CLI prints them in that iteration order. Because `HashMap` iteration is randomized, successive runs of the same opening emit node statuses in different orders (e.g., `review` before `contacts`).

### Impact

- Harder to eyeball regressions in CLI runs because the table reshuffles each invocation.
- Replay comparisons require extra diff noise filtering to spot real differences.

### Root Cause

The report drains `records.into_values()` instead of preserving the topological or declared node order.

### Recommended Fix

Collect records in `opening.nodes` order (or perform a topological sort) before returning them, ensuring deterministic CLI output and trace serialization.

## 9. Lockfile References Non-Existent `serde_core` Crate

- **Status:** Closed — false positive (2025-11-07)  
- **Scope:** `Cargo.lock`

### What Happens

`Cargo.lock` now declares multiple dependencies on a crate named `serde_core` (e.g., under `camino`, `semver`, and `serde_json`), and even contains a `[[package]] name = "serde_core"` stanza. Crates.io does not publish such a crate, so `cargo metadata --locked`, `cargo fetch`, and all build/test commands fail immediately with “no matching package named `serde_core` found.”

### Impact

- Completely blocks `cargo build`, `cargo test`, and CI until the lockfile is repaired.  
- Developers cannot run `cargo fmt`/`cargo clippy` because Cargo refuses to resolve dependencies.  
- Automation (packaging, release, CI) is dead in the water, halting progress on every crate.

### Root Cause

The lockfile was edited manually (or by a broken tool) to replace `serde` with the non-existent `serde_core`. No corresponding `Cargo.toml` entry references that package, so the resolver cannot satisfy the dependency graph.

### Recommended Fix

Regenerate the lockfile from authoritative manifests—e.g., run `cargo update -p serde` (or `cargo update`) to restore the proper `serde` package entries. Commit the regenerated `Cargo.lock` so CI and developers can resolve dependencies again.

### Resolution

`cargo generate-lockfile` now succeeds with the current workspace manifests, and `cargo metadata --locked` runs cleanly. The `serde_core` crate (v1.0.228) is an official crates.io package that several dependencies enable, so the lockfile is valid and no further action is required.

## 10. Remote IPC Clients Hang On Socket Disconnects

- **Status:** Fixed (2025-11-07)  
- **Scope:** `crates/bus/src/ipc/unix.rs:148-255`

### What Happens

When the Unix socket is closed or the runtime restarts, `read_frames` exits its loop without flushing any of the outstanding `PendingAck` entries for publishes/subscribes. The callers are all awaiting a `oneshot` that never receives a value, so the corresponding `shim.publish(..)` / `shim.subscribe(..)` futures hang forever and never surface an error.

### Impact

- Native agents that lose their bus connection deadlock instead of reconnecting, wedging openings.  
- Supervisors can’t detect the outage because the shim never returns a `BusError`.  
- A single transient socket blip requires a full process restart to clear stuck awaiters.

### Root Cause

`read_frames` simply `break`s on any `read_exact` failure without iterating over `inner.pending` to send `Err(BusError::Closed)` to each stored oneshot. The `subscriptions` map is also left populated, leaking channels that will never be serviced again.

### Recommended Fix

Before returning from `read_frames`, drain `pending` and send `Err(BusError::Closed)` (or `BusError::NotFound`) to every waiter, and remove all subscriptions. That allows higher layers to observe the disconnect and attempt a clean reconnect.

### Resolution

`read_frames` now always drains `pending` acks and active subscription channels via `IpcClientInner::teardown` before exiting, ensuring all waiters observe `BusError::Closed` and subscribers receive channel closures.

## 11. Slow Subscriber Can Deadlock IPC Reader

- **Status:** Fixed (2025-11-07)  
- **Scope:** `crates/bus/src/ipc/unix.rs:226-235`

### What Happens

For each `IpcEvent::Message`, the client tries `tx.try_send`. If the channel is full, it calls `tx.send(msg).await` **inside the reader task**, with no timeout or buffering. If that subscriber stops polling, the await never resolves and the reader stops processing frames entirely—blocking publish acks, subscribe acks, and all other subscriptions on that connection.

### Impact

- One misbehaving native agent can wedge every other subscriber on the same shim.  
- Pending publish operations never get their acks, so the caller believes the bus hung.  
- Backpressure semantics diverge from the in-process bus (which enforces timeouts and drop notices), making remote behavior harder to reason about.

### Root Cause

The reader task multiplexes both control and data traffic on a single async loop. Awaiting `tx.send` inside that loop means message delivery is no longer fair; the backlog of a single receiver blocks processing of unrelated frames.

### Recommended Fix

Either spawn per-subscription forwarding tasks (so a blocked subscriber only stalls itself) or mirror the bus’s timeout semantics by cancelling the send after `config.send_timeout` and dropping the subscriber. At minimum, ensure the reader loop never awaits on per-subscriber backpressure.

### Resolution

Message forwarding now clones the `Sender` and pushes blocking sends into background tasks; stalled subscribers are dropped via `cleanup_subscription`, so the reader task never awaits on per-subscriber backpressure.

## 12. Protocol Docs Still Cite 60-Byte Header

- **Status:** Fixed (2025-11-07)  
- **Scope:** `DESCRIPTION.md:60-70`

### What Happens

The high-level protocol description now says “fixed 68-byte header” in the bullet list but immediately lists `header_len=60` in the parenthetical field summary. Readers comparing this description with `README.md`/`docs/message-protocol.md` (and the actual `runloop-rmp` implementation) get conflicting numbers for the header size.

### Impact

- External consumers implementing the wire protocol may allocate the wrong header size.  
- Documentation reviewers waste time reconciling contradictory specs.  
- Undermines confidence that the README/specs are authoritative.

### Root Cause

The header length was bumped to 68 bytes when `opening_id` became a `u128`, but `DESCRIPTION.md` still mentions the old `header_len=60` constant.

### Recommended Fix

Update `DESCRIPTION.md` to state `header_len=68`, matching the code and other docs. Optionally add a short note explaining that `opening_id` is now 16 bytes to prevent regressions.

### Resolution

`DESCRIPTION.md` now lists `header_len=68`, matching `runloop-rmp` and the rest of the protocol docs.
