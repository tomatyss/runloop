# Runtime Bug Backlog

The new `runloop-runtime` crate introduces a richer Wasmtime embedding, but
several high-priority defects remain outstanding. Each issue below captures the
observed behavior, impact, suspected root cause, and recommended next actions.

## 1. Stdout/Stderr Buffers Grow Without Bound

- **Status:** High
- **Scope:** `crates/runtime/src/runtime.rs:90-114,334-340`

### What Happens

`RingWriter::write` pushes every byte into both the bounded `OutputRing` and an
auxiliary `Vec<u8>` protected by `RwLock`. These vectors are never truncated or
exposed, so they grow for the lifetime of the agent.

### Impact

- A noisy or malicious guest can consume unbounded host memory.
- Long-lived agents leak memory proportional to their log volume.

### Root Cause

The `_stdout_buffer` and `_stderr_buffer` fields exist but no consumer drains
them. They accumulate log data indefinitely.

### Recommended Fix

Either remove the unbounded buffers entirely, or enforce a capacity (matching
the ring) and drop old data when full.

## 2. Module Cache Ignores On-Disk Updates

- **Status:** Medium
- **Scope:** `crates/runtime/src/module_cache.rs:23-31`

### What Happens

`ModuleCache::load` stores every compiled `Module` in a
`DashMap<PathBuf, Arc<Module>>` and never invalidates entries. Once cached,
future calls return the old module even if the Wasm binary has been modified on
disk.

### Impact

- Operators cannot deploy new Wasm builds without restarting the entire runtime.
- Hot-reload workflows silently run stale code, risking user-facing regressions.

### Root Cause

The cache key is the raw path, and there is no mtime/hash check to detect
changes.

### Recommended Fix

Track file metadata (e.g., mtime + length) or a content hash alongside the
cached module. If the file changes, evict and recompile before returning the
module.

## 3. Bus Sends Panic Inside Nested Tokio Runtimes

- **Status:** High
- **Scope:** `crates/runtime/src/runtime.rs:493-505`

### What Happens

When `Runtime::send` runs inside an existing Tokio executor, the call to
`spawner.block_on` invokes `TokioHandle::block_on`, which panics with "cannot
start a runtime from within a runtime." Any caller that embeds the runtime
inside async services hits this panic the first time it forwards a bus message.

### Impact

- Crashes supervising services that dispatch messages from async contexts.
- Prevents integrating the runtime into async control planes without wrapping
  every `send` in a separate thread.
- Masks the underlying bus error semantics because the panic aborts execution
  paths.

### Root Cause

The new bus integration replaced the non-async `inbox.try_send` path with a hard
`block_on` of the async bus send operation, ignoring Tokio's
`!Send`-from-runtime constraints.

### Recommended Fix

Offload the async send without calling `block_on` on the current handle—e.g.,
spawn a task that writes to the bus and synchronizes via a oneshot, or fall back
to the in-process channel when the runtime already has direct access to the
agent inbox.

## 4. `model_complete` Corrupts Prompt Buffer When Reserving Output Space

- **Status:** High
- **Scope:** `crates/runtime/src/hostcalls.rs:314-345`

### What Happens

The hostcall treats `prompt_len` as both the size of the prompt to read and the
available capacity for writing the model output. Guests that pass a buffer
larger than the actual prompt (to leave room for the response) have their prompt
string read with trailing zeroed padding, which the echo broker then mirrors
back.

### Impact

- Model prompts delivered to the broker include garbage bytes, causing
  nondeterministic completions.
- There is no way for guests to request responses longer than the prompt without
  contaminating their request payload.
- Future, non-echo providers will misinterpret prompts and return incorrect or
  rejected completions.

### Root Cause

`read_utf8` consumes `prompt_len` bytes before the host knows how much of that
buffer contains initialized prompt data. The API conflates "prompt length" with
"output capacity."

### Recommended Fix

Extend the hostcall signature to separate the prompt input length from the
output buffer capacity (or write into a distinct pointer). The host should read
only the initialized prompt slice and use an explicit capacity when checking for
response truncation.

## 5. Predicate Comparisons Overflow On Large Unsigned Values

- **Status:** High
- **Scope:** `crates/openings/src/runner.rs:216-254`

### What Happens

`evaluate_predicate` handles integer literals by first checking `num.as_i64()`,
then falling back to `num.as_u64()` and casting the result to `i64`. Values
above `i64::MAX` therefore wrap into the negative range before being compared,
so predicates like `foo.score >= 9223372036854775808` evaluate against a
negative number.

### Impact

- Openings that gate on monotonically increasing counters or IDs (e.g.,
  Snowflake IDs) report false negatives.
- Success conditions tied to large numeric thresholds can be bypassed because
  the comparison sees wrapped values.
- Replay traces drift from real world runs when large unsigned outputs appear.

### Root Cause

The integer code path truncates `u64` values to `i64` instead of keeping the
comparison in an unsigned domain (or promoting both sides to `f64`/BigInt).

### Recommended Fix

Preserve unsigned comparisons—either compare using `u128`/`u64` throughout or
branch on the literal type so `Literal::Integer` drives
`as_i128`/`as_u128`-aware logic without lossy casts.

## 6. Default Config Emits Spurious Missing-Directory Warnings

- **Status:** Medium
- **Scope:** `crates/core/src/config.rs:48-134`

### What Happens

`Config::validate` now calls `warn_missing_search_dirs` for openings and agents.
The new defaults include repo-relative paths (`./examples/openings`, `./agents`)
and user directories (`~/.runloop/*`) that typically do not exist on a fresh
install, so every CLI invocation prints multiple WARN lines.

### Impact

- Users see noisy warnings even when nothing is misconfigured, reducing trust in
  diagnostics.
- CI and scripted runs that treat WARN as noteworthy now look degraded after
  this change.

### Root Cause

The helper unconditionally warns on missing directories, including those
injected by the default configuration instead of user-provided overrides.

### Recommended Fix

Downgrade the log level (e.g., to debug) for default paths, or emit warnings
only for directories supplied via config files/env overrides. Alternatively,
create the repo-relative directories during validation instead of warning.

## 7. Node Status Output Order Is Non-Deterministic

- **Status:** Low
- **Scope:** `crates/openings/src/runner.rs:170-212`

### What Happens

`RunReport` collects `node_records` from a `HashMap`, and the CLI prints them in
that iteration order. Because `HashMap` iteration is randomized, successive runs
of the same opening emit node statuses in different orders (e.g., `review`
before `contacts`).

### Impact

- Harder to eyeball regressions in CLI runs because the table reshuffles each
  invocation.
- Replay comparisons require extra diff noise filtering to spot real
  differences.

### Root Cause

The report drains `records.into_values()` instead of preserving the topological
or declared node order.

### Recommended Fix

Collect records in `opening.nodes` order (or perform a topological sort) before
returning them, ensuring deterministic CLI output and trace serialization.

## 8. KB Write Denials Are Misclassified As Read Denials

- **Status:** High
- **Scope:** `crates/runtime/src/error.rs:21-42`

### What Happens

`CapKind::from_label` truncates labels at the first `.` before matching the
prefix. Labels like `kb.write.repo` therefore collapse to `kb` and fall through
to the `KbRead` arm. Structured errors emitted through `CapDeniedInfo` and
surfaced via `Error::CapDenied` will always report `cap=kb_read`, even when a
write namespace triggered the denial.

### Impact

- Audit trails and metrics can no longer distinguish read vs. write policy
  violations.
- Alerts for write-only regressions never fire because those denials are
  misbucketed under the read counters.
- Automated remediations that depend on the structured `cap` field become
  unreliable.

### Root Cause

`CapKind::from_label` never checks for `kb.write` before truncating, so the
`KbWrite` variant is effectively unreachable.

### Recommended Fix

Match the full label before stripping suffixes (or only split when the full
label is unknown) so `kb.write.*` resolves to `CapKind::KbWrite` while retaining
prefix matching for other capability families.

## 9. Agents That Only Call `mailbox_peek_meta` Never Reach Ready State

- **Status:** High
- **Scope:** `crates/runtime/src/hostcalls.rs:111-117,417-420` and
  `crates/runtime/src/runtime.rs:298-365`

### What Happens

The readiness barrier introduced for `Runtime::spawn` only listens for
`runloop::notify_ready` or a `mailbox_recv` invocation. Legacy agents that idle
by polling `mailbox_peek_meta` (and only call `mailbox_recv` after they see a
header) never exercise either code path, so `spawn` waits until
`spawn_ready_timeout_ms` elapses and returns `Error::ReadyTimeout` even though
`_start` succeeded and the agent is idle.

### Impact

- Peek-first agents can no longer be supervised; every launch times out.
- Operators must either disable the readiness gate (defeating the feature) or
  rebuild every agent to add `notify_ready`, slowing staged rollouts.
- The documented fallback path for "pre-ready binaries" is incomplete, making
  the regression hard to diagnose.

### Root Cause

`host_mailbox_recv` calls `HostState::notify_mailbox_recv`, but
`mailbox_peek_meta` never does, so the readiness emitter is never tripped when
guests remain in peek mode.

### Recommended Fix

Treat `mailbox_peek_meta` as a readiness signal (either on the first invocation
or once it returns a header), or add a small hostcall that legacy agents can
invoke before entering their peek loop.

## 10. `context_gatherer` Crashes Because the Views DB Lacks an `events` Table

- **Status:** High
- **Scope:** `crates/agents/context_gatherer/src/lib.rs:24-55`,
  `crates/kb/src/lib.rs:780-870`

### What Happens

`ContextGatherer` calls
`kb.query("SELECT id, kind, payload_json FROM events …")`, but
`KnowledgeBase::query` executes against the _views_ connection. The views
database only materializes contacts, artifacts, runs, etc., and never creates an
`events` table, so every run hits `SqliteFailure("no such table: events")`
before any fallback snippet is produced.

### Impact

- The compose-email opening fails at the `context` node even when the KB is
  otherwise healthy.
- The CLI run path added in this PR can never succeed because `context_gatherer`
  panics immediately.

### Root Cause

The agent assumes it can read raw events via the views handle, but only the
ledger database contains that table. `kb.query` is the wrong API for the ledger.

### Recommended Fix

Either (a) materialize the needed event projection into the views DB (so
`kb.query` works), or (b) expose/read via a dedicated helper that runs against
the ledger connection (e.g., `kb.scan_events(...)`).

## 11. CLI Swallows Type Errors for `recipient_query`

- **Status:** Medium
- **Scope:** `crates/rlp/src/main.rs:243-249`

### What Happens

`exec_contact` reads the node parameter twice and calls `.unwrap_or(None)` on
the second attempt. If deserialization fails (e.g., the YAML passes a number),
the error is dropped and the executor falls back to the default `""`, which then
triggers the generic "contact_resolver requires 'query'" message.

### Impact

- Misconfigured openings surface as empty-query errors, hiding the _real_ type
  mismatch.
- Users cannot tell which parameter is malformed, slowing iteration on DAG
  templates.

### Root Cause

The code treats the `Result<Option<T>>` from `node_param` as an
`Option<Option<T>>` and calls `unwrap_or(None)`, bypassing the `Err` state
altogether.

### Recommended Fix

Propagate the `RunnerError` returned by `node_param` instead of discarding it,
so bad user input results in a clear "invalid 'recipient_query' param" error.

## 12. Context Filters Ignore Case and Allow Accidental Wildcards

- **Status:** Medium
- **Scope:** `crates/agents/context_gatherer/src/lib.rs:24-41`

### What Happens

The agent lowercases the _query_ but not the `payload_json` column, then
interpolates both queries and topics directly into a `LIKE '%{value}%'` string
without escaping `%` or `_`. Case-insensitive matches therefore miss whenever
the stored JSON contains uppercase text, and `%` / `_` characters in user input
unexpectedly broaden the search.

### Impact

- Context gathering routinely returns the fallback snippet even when matching
  events exist.
- Malicious templates can inject extra `%` wildcards, forcing a table scan over
  the entire events log.

### Root Cause

The SQL fragments neither normalize the column (e.g., `LOWER(payload_json)`) nor
escape user-provided values, so the predicate is both case-sensitive and
vulnerable to wildcard expansion.

### Recommended Fix

Wrap the column with `LOWER(...)` and use parameter binding/escaping for `%` and
`_` so searches are truly case-insensitive and incapable of altering the pattern
structure.

## 13. Predicate-Gated Nodes Can Run Before Guards Pass

- **Status:** High
- **Scope:** `crates/openings/src/runner.rs:300-339`,
  `examples/openings/compose_email.yaml:30-55`

### What Happens

When a node has multiple inbound edges, the runner enqueues it as soon as
**any** upstream port emits a value, even if another inbound edge carries the
predicate being used as a guard (e.g., `review.ok==true -> send.in`). After
wiring `contacts.out`, `draft.out`, and `review.review` into the mailer, `send`
now starts executing the moment the contacts edge fires, long before the Boolean
predicate is satisfied. The mailer then sees an unapproved draft and fails with
"draft not approved," which incorrectly aborts the run while still prompting the
operator.

### Impact

- Predicate-controlled fan-in graphs no longer behave deterministically; gated
  nodes can fire out of order and spam downstream services.
- Compose-email now fails for the wrong reason (mailer rejection) instead of
  simply skipping the send node when the critic blocks it.

### Root Cause

`Runner::run` only checks `delivered` (whether any inputs exist) when the
inbound edge counter reaches zero. It never verifies that predicate edges
actually produced a value, so boolean guards are treated the same as optional
data edges.

### Recommended Fix

Track predicate-bearing edges separately: require at least one value on every
predicate port before enqueuing the downstream node, or defer decrementing
`remaining` until the predicate evaluates to true. That guarantees
predicate-controlled nodes cannot start until their guards pass.

## 14. `rlp replay` Re-executes Agents with Real Side Effects

- **Status:** Medium
- **Scope:** `crates/rlp/src/main.rs:492-520,593-605`,
  `crates/openings/src/replay.rs:75-125`

### What Happens

Replaying a trace reconstructs the full `LocalExecutor` (broker, confirmation
provider, KB writes) and then invokes every agent again via `executor.execute`.
That causes the writer to generate new drafts, the mailer to solicit
confirmation (or send mail if confirmations are disabled), and the KB to
accumulate duplicate events—all while the operator is merely trying to verify
determinism.

### Impact

- `rlp replay` is unusable in CI or production debugging because it mutates
  state and can trigger external effects (emails, model usage, tokens).
- Determinism checks become flaky: replays may diverge purely because a model
  call returned different text the second time.

### Root Cause

The replay path shares the same executor implementation used for live runs, and
`replay.rs` doesn’t provide recorded outputs to short-circuit agent execution.
Instead, it calls `executor.execute` for each node, which faithfully replays
side effects rather than comparing the stored outputs.

### Recommended Fix

Introduce a deterministic, side-effect-free executor for replay (e.g., one that
returns the recorded outputs directly, or mocks external services) so
verification never calls real agents, touches the KB, or prompts the operator.
