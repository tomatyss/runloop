# Runtime Bug Backlog

The new `runloop-runtime` crate introduces a richer Wasmtime embedding, but
several high-priority defects remain outstanding. Each issue below captures the
observed behavior, impact, suspected root cause, and recommended next actions.

## 1. Stdout/Stderr Buffers Grow Without Bound

- **Status:** High
- **Scope:** `crates/runtime/src/runtime.rs:90-114,334-340`

### 1.1 What Happens

`RingWriter::write` pushes every byte into both the bounded `OutputRing` and an
auxiliary `Vec<u8>` protected by `RwLock`. These vectors are never truncated or
exposed, so they grow for the lifetime of the agent.

### 1.2 Impact

- A noisy or malicious guest can consume unbounded host memory.
- Long-lived agents leak memory proportional to their log volume.

### 1.3 Root Cause

The `_stdout_buffer` and `_stderr_buffer` fields exist but no consumer drains
them. They accumulate log data indefinitely.

### 1.4 Recommended Fix

Either remove the unbounded buffers entirely, or enforce a capacity (matching
the ring) and drop old data when full.

## 6. Default Config Emits Spurious Missing-Directory Warnings

- **Status:** Medium
- **Scope:** `crates/core/src/config.rs:48-134`

### 6.1 What Happens

`Config::validate` now calls `warn_missing_search_dirs` for openings and agents.
The new defaults include repo-relative paths (`./examples/openings`, `./agents`)
and user directories (`~/.runloop/*`) that typically do not exist on a fresh
install, so every CLI invocation prints multiple WARN lines.

### 6.2 Impact

- Users see noisy warnings even when nothing is misconfigured, reducing trust in
  diagnostics.
- CI and scripted runs that treat WARN as noteworthy now look degraded after
  this change.

### 6.3 Root Cause

The helper unconditionally warns on missing directories, including those
injected by the default configuration instead of user-provided overrides.

### 6.4 Recommended Fix

Downgrade the log level (e.g., to debug) for default paths, or emit warnings
only for directories supplied via config files/env overrides. Alternatively,
create the repo-relative directories during validation instead of warning.

## 7. Node Status Output Order Is Non-Deterministic

- **Status:** Low
- **Scope:** `crates/openings/src/runner.rs:170-212`

### 7.1 What Happens

`RunReport` collects `node_records` from a `HashMap`, and the CLI prints them in
that iteration order. Because `HashMap` iteration is randomized, successive runs
of the same opening emit node statuses in different orders (e.g., `review`
before `contacts`).

### 7.2 Impact

- Harder to eyeball regressions in CLI runs because the table reshuffles each
  invocation.
- Replay comparisons require extra diff noise filtering to spot real
  differences.

### 7.3 Root Cause

The report drains `records.into_values()` instead of preserving the topological
or declared node order.

### 7.4 Recommended Fix

Collect records in `opening.nodes` order (or perform a topological sort) before
returning them, ensuring deterministic CLI output and trace serialization.

## 8. KB Write Denials Are Misclassified As Read Denials

- **Status:** High
- **Scope:** `crates/runtime/src/error.rs:21-42`

### 8.1 What Happens

`CapKind::from_label` truncates labels at the first `.` before matching the
prefix. Labels like `kb.write.repo` therefore collapse to `kb` and fall through
to the `KbRead` arm. Structured errors emitted through `CapDeniedInfo` and
surfaced via `Error::CapDenied` will always report `cap=kb_read`, even when a
write namespace triggered the denial.

### 8.2 Impact

- Audit trails and metrics can no longer distinguish read vs. write policy
  violations.
- Alerts for write-only regressions never fire because those denials are
  misbucketed under the read counters.
- Automated remediations that depend on the structured `cap` field become
  unreliable.

### 8.3 Root Cause

`CapKind::from_label` never checks for `kb.write` before truncating, so the
`KbWrite` variant is effectively unreachable.

### 8.4 Recommended Fix

Match the full label before stripping suffixes (or only split when the full
label is unknown) so `kb.write.*` resolves to `CapKind::KbWrite` while retaining
prefix matching for other capability families.

## 9. Agents That Only Call `mailbox_peek_meta` Never Reach Ready State

- **Status:** High
- **Scope:** `crates/runtime/src/hostcalls.rs:111-117,417-420` and
  `crates/runtime/src/runtime.rs:298-365`

### 9.1 What Happens

The readiness barrier introduced for `Runtime::spawn` only listens for
`runloop::notify_ready` or a `mailbox_recv` invocation. Legacy agents that idle
by polling `mailbox_peek_meta` (and only call `mailbox_recv` after they see a
header) never exercise either code path, so `spawn` waits until
`spawn_ready_timeout_ms` elapses and returns `Error::ReadyTimeout` even though
`_start` succeeded and the agent is idle.

### 9.2 Impact

- Peek-first agents can no longer be supervised; every launch times out.
- Operators must either disable the readiness gate (defeating the feature) or
  rebuild every agent to add `notify_ready`, slowing staged rollouts.
- The documented fallback path for "pre-ready binaries" is incomplete, making
  the regression hard to diagnose.

### 9.3 Root Cause

`host_mailbox_recv` calls `HostState::notify_mailbox_recv`, but
`mailbox_peek_meta` never does, so the readiness emitter is never tripped when
guests remain in peek mode.

### 9.4 Recommended Fix

Treat `mailbox_peek_meta` as a readiness signal (either on the first invocation
or once it returns a header), or add a small hostcall that legacy agents can
invoke before entering their peek loop.

## 10. `context_gatherer` Crashes Because the Views DB Lacks an `events` Table

- **Status:** High
- **Scope:** `crates/agents/context_gatherer/src/lib.rs:24-55`,
  `crates/kb/src/lib.rs:780-870`

### 10.1 What Happens

`ContextGatherer` calls
`kb.query("SELECT id, kind, payload_json FROM events …")`, but
`KnowledgeBase::query` executes against the _views_ connection. The views
database only materializes contacts, artifacts, runs, etc., and never creates an
`events` table, so every run hits `SqliteFailure("no such table: events")`
before any fallback snippet is produced.

### 10.2 Impact

- The compose-email opening fails at the `context` node even when the KB is
  otherwise healthy.
- The CLI run path added in this PR can never succeed because `context_gatherer`
  panics immediately.

### 10.3 Root Cause

The agent assumes it can read raw events via the views handle, but only the
ledger database contains that table. `kb.query` is the wrong API for the ledger.

### 10.4 Recommended Fix

Either (a) materialize the needed event projection into the views DB (so
`kb.query` works), or (b) expose/read via a dedicated helper that runs against
the ledger connection (e.g., `kb.scan_events(...)`).

## 11. CLI Swallows Type Errors for `recipient_query`

- **Status:** Medium
- **Scope:** `crates/rlp/src/main.rs:243-249`

### 11.1 What Happens

`exec_contact` reads the node parameter twice and calls `.unwrap_or(None)` on
the second attempt. If deserialization fails (e.g., the YAML passes a number),
the error is dropped and the executor falls back to the default `""`, which then
triggers the generic "contact_resolver requires 'query'" message.

### 11.2 Impact

- Misconfigured openings surface as empty-query errors, hiding the _real_ type
  mismatch.
- Users cannot tell which parameter is malformed, slowing iteration on DAG
  templates.

### 11.3 Root Cause

The code treats the `Result<Option<T>>` from `node_param` as an
`Option<Option<T>>` and calls `unwrap_or(None)`, bypassing the `Err` state
altogether.

### 11.4 Recommended Fix

Propagate the `RunnerError` returned by `node_param` instead of discarding it,
so bad user input results in a clear "invalid 'recipient_query' param" error.

## 12. Context Filters Ignore Case and Allow Accidental Wildcards

- **Status:** Medium
- **Scope:** `crates/agents/context_gatherer/src/lib.rs:24-41`

### 12.1 What Happens

The agent lowercases the _query_ but not the `payload_json` column, then
interpolates both queries and topics directly into a `LIKE '%{value}%'` string
without escaping `%` or `_`. Case-insensitive matches therefore miss whenever
the stored JSON contains uppercase text, and `%` / `_` characters in user input
unexpectedly broaden the search.

### 12.2 Impact

- Context gathering routinely returns the fallback snippet even when matching
  events exist.
- Malicious templates can inject extra `%` wildcards, forcing a table scan over
  the entire events log.

### 12.3 Root Cause

The SQL fragments neither normalize the column (e.g., `LOWER(payload_json)`) nor
escape user-provided values, so the predicate is both case-sensitive and
vulnerable to wildcard expansion.

### 12.4 Recommended Fix

Wrap the column with `LOWER(...)` and use parameter binding/escaping for `%` and
`_` so searches are truly case-insensitive and incapable of altering the pattern
structure.

## 13. Predicate-Gated Nodes Can Run Before Guards Pass

- **Status:** High
- **Scope:** `crates/openings/src/runner.rs:300-339`,
  `examples/openings/compose_email.yaml:30-55`

### 13.1 What Happens

When a node has multiple inbound edges, the runner enqueues it as soon as
**any** upstream port emits a value, even if another inbound edge carries the
predicate being used as a guard (e.g., `review.ok==true -> send.in`). After
wiring `contacts.out`, `draft.out`, and `review.review` into the mailer, `send`
now starts executing the moment the contacts edge fires, long before the Boolean
predicate is satisfied. The mailer then sees an unapproved draft and fails with
"draft not approved," which incorrectly aborts the run while still prompting the
operator.

### 13.2 Impact

- Predicate-controlled fan-in graphs no longer behave deterministically; gated
  nodes can fire out of order and spam downstream services.
- Compose-email now fails for the wrong reason (mailer rejection) instead of
  simply skipping the send node when the critic blocks it.

### 13.3 Root Cause

`Runner::run` only checks `delivered` (whether any inputs exist) when the
inbound edge counter reaches zero. It never verifies that predicate edges
actually produced a value, so boolean guards are treated the same as optional
data edges.

### 13.4 Recommended Fix

Track predicate-bearing edges separately: require at least one value on every
predicate port before enqueuing the downstream node, or defer decrementing
`remaining` until the predicate evaluates to true. That guarantees
predicate-controlled nodes cannot start until their guards pass.

## 14. `rlp replay` Re-executes Agents with Real Side Effects

- **Status:** Medium
- **Scope:** `crates/rlp/src/main.rs:492-520,593-605`,
  `crates/openings/src/replay.rs:75-125`

### 14.1 What Happens

Replaying a trace reconstructs the full `LocalExecutor` (broker, confirmation
provider, KB writes) and then invokes every agent again via `executor.execute`.
That causes the writer to generate new drafts, the mailer to solicit
confirmation (or send mail if confirmations are disabled), and the KB to
accumulate duplicate events—all while the operator is merely trying to verify
determinism.

### 14.2 Impact

- `rlp replay` is unusable in CI or production debugging because it mutates
  state and can trigger external effects (emails, model usage, tokens).
- Determinism checks become flaky: replays may diverge purely because a model
  call returned different text the second time.

### 14.3 Root Cause

The replay path shares the same executor implementation used for live runs, and
`replay.rs` doesn’t provide recorded outputs to short-circuit agent execution.
Instead, it calls `executor.execute` for each node, which faithfully replays
side effects rather than comparing the stored outputs.

### 14.4 Recommended Fix

Introduce a deterministic, side-effect-free executor for replay (e.g., one that
returns the recorded outputs directly, or mocks external services) so
verification never calls real agents, touches the KB, or prompts the operator.
