# Runloop OS Engineering Standards

This document defines the technical standards, architectural guidelines, and
decision-making framework for Runloop OS development. It is the authoritative
reference for code quality expectations.

**Related documents:**
- [CONTRIBUTING.md](../CONTRIBUTING.md) — Process, workflow, DCO
- [AGENTS.md](../AGENTS.md) — Quick reference for AI coding agents
- [Contributor Guide](contributor-guide.md) — Onboarding, issue selection

---

## Table of Contents

1. [Project Objectives](#1-project-objectives)
2. [Decision-Making Framework](#2-decision-making-framework)
3. [Architectural Guidelines](#3-architectural-guidelines)
4. [Code Size Limits](#4-code-size-limits)
5. [Rust Standards](#5-rust-standards)
6. [Testing Standards](#6-testing-standards)
7. [Documentation Standards](#7-documentation-standards)
8. [Performance Guidelines](#8-performance-guidelines)
9. [Dependency Management](#9-dependency-management)
10. [Logging and Observability](#10-logging-and-observability)
11. [API Design Guidelines](#11-api-design-guidelines)
12. [Code Review Checklist](#12-code-review-checklist)
13. [Git and PR Workflow](#13-git-and-pr-workflow)
14. [Exemplary Patterns in the Codebase](#14-exemplary-patterns-in-the-codebase)
15. [Anti-Patterns to Avoid](#15-anti-patterns-to-avoid)
16. [Quick Reference Card](#16-quick-reference-card)

---

## 1. Project Objectives

Every technical decision must be evaluated against these priorities **in order**:

### Priority 1: Reliability

The system must not lose data or fail silently. Prefer boring, proven patterns
over clever solutions. A 10x slower approach that never fails beats a fast one
that fails 0.1% of the time.

**Indicators:**
- All error paths are handled explicitly
- State changes are atomic or recoverable
- Failures are visible (logs, metrics, alerts)

### Priority 2: Security

This runs as a system service with access to user data and external APIs. Assume
adversarial inputs. Capability enforcement is not optional.

**Indicators:**
- Input validation at all system boundaries
- Principle of least privilege for agents
- No secrets in logs or error messages
- Capability denials are audited

### Priority 3: Debuggability

When (not if) something goes wrong in production, can you figure out what
happened? Structured logs, traces, and deterministic replay matter more than raw
performance.

**Indicators:**
- Every operation has a trace_id
- Errors include context (file, line, inputs)
- KB event log enables replay
- Metrics expose internal state

### Priority 4: Maintainability

Code will be read 10x more than written. Optimize for the next developer (who
may be you in 6 months with no context).

**Indicators:**
- Functions fit on one screen
- Names are descriptive, not abbreviated
- Complex logic has comments explaining "why"
- Public APIs are documented

### Priority 5: Performance

Only after the above are satisfied. Profile before optimizing. Benchmark claims
require reproducible numbers.

**Indicators:**
- Hot paths identified via profiling
- Benchmarks exist for critical operations
- Optimization PRs include before/after numbers

---

## 2. Decision-Making Framework

### The Runloop Decision Test

When facing a technical choice, ask these questions in order:

#### Question 1: "What happens when this fails?"

```
BAD:  "It won't fail"
      Wrong answer. Everything fails.

GOOD: "It returns Err(...) and the caller retries/logs/propagates"
      "The operation is idempotent, so retry is safe"
      "We emit a metric and degrade gracefully"
```

#### Question 2: "Can I debug this at 3 AM with only logs?"

```
BAD:  "You'd need to attach a debugger"
      Unacceptable for production.

BAD:  "It's obvious from the code"
      You won't have code context at 3 AM.

GOOD: "The trace_id connects all related log lines"
      "The error includes file:line and input context"
      "We can replay from the KB event log"
```

#### Question 3: "Will this still work in 2 years?"

```
BAD:  "We can refactor later"
      Technical debt accrues interest.

BAD:  "Only I understand this"
      Bus factor of 1.

GOOD: "The API is stable and versioned"
      "The config schema has defaults for new fields"
      "A new team member can understand this from docs"
```

#### Question 4: "What's the simplest thing that works?"

```
BAD:  "I added a framework for future flexibility"
BAD:  "This handles edge cases we might need"

GOOD: "It does exactly what's required, no more"
      "I can explain it in one sentence"
```

### Trade-off Decision Matrix

When two valid approaches exist, score them:

| Factor | Weight | Option A | Option B |
|--------|--------|----------|----------|
| Reliability | 5 | ? / 5 | ? / 5 |
| Security | 4 | ? / 5 | ? / 5 |
| Debuggability | 3 | ? / 5 | ? / 5 |
| Maintainability | 3 | ? / 5 | ? / 5 |
| Performance | 2 | ? / 5 | ? / 5 |
| **Weighted Total** | | | |

Document this in the PR description for significant architectural decisions.

### Common Decision Patterns

#### "Should I use a trait or concrete type?"

**Use TRAIT when:**
- Multiple implementations exist today
- Testing requires mocking external dependencies
- External crates will implement it
- The abstraction is well-understood (Executor, SecretProvider)

**Use CONCRETE TYPE when:**
- Only one implementation exists
- "Maybe we'll need it" is the only reason for abstraction
- The type is internal to one crate

#### "Should I handle this error or propagate it?"

**HANDLE (recover/retry) when:**
- You have enough context to fix it
- The error is expected in normal operation
- Retrying might succeed (network, lock contention)

**PROPAGATE (`?`) when:**
- Caller has more context
- Error is unrecoverable at this level
- You're in library code (let app decide)

**LOG AND PROPAGATE when:**
- You're crossing a major boundary (crate, async spawn)
- Context would be lost without logging
- Error is unexpected (bug, corruption)

#### "Should I clone or restructure?"

**CLONE when:**
- Data is small (< 1KB)
- Called infrequently (< 100/sec)
- Restructuring would complicate the API significantly
- You need owned data for `Send` across threads

**RESTRUCTURE when:**
- Clone appears in a hot loop
- Data is large (buffers, collections)
- The borrow checker complaint reveals a design issue
- You can use references with lifetime annotations

#### "Should I add a config option or hardcode?"

**CONFIG OPTION when:**
- Different deployments need different values
- Operators need to tune for their environment
- The default might be wrong for some use cases

**HARDCODE when:**
- Changing it would break invariants
- It's an implementation detail
- "Just in case" is the only reason
- Document the constant with rationale

---

## 3. Architectural Guidelines

### Crate Organization

The workspace follows a layered architecture:

```
                    +------------------+
                    |   runloop-core   |  <- Shared types, IDs, config
                    +--------+---------+
                             |
         +-------------------+-------------------+
         |                   |                   |
         v                   v                   v
   +-----------+      +-----------+      +-----------+
   |runloop-bus|      |runloop-kb |      |runloop-rmp|
   +-----+-----+      +-----+-----+      +-----------+
         |                  |
         +--------+---------+
                  |
         +--------v--------+
         | runloop-runtime |  <- Depends on bus + kb
         +--------+--------+
                  |
         +--------v--------+
         |runloop-openings |  <- Depends on runtime
         +--------+--------+
                  |
    +-------------+-------------+
    |             |             |
    v             v             v
+-------+    +--------+    +-------+
|runloopd|   |  rlp   |    | agtop |  <- Binaries at leaf
+-------+    +--------+    +-------+
```

### When to Create a New Crate

**Create a new crate when:**

| Signal | Example |
|--------|---------|
| Separate compilation unit needed | WASM agents must be separate crates |
| Different dependency tree | `agtop` needs `ratatui`, daemon doesn't |
| Reusable by external consumers | `runloop-rmp` protocol for third-party tools |
| Different lint/feature requirements | WASM SDK needs `#![allow(unsafe_code)]` |
| Clear domain boundary | KB, Bus, Runtime are distinct subsystems |

**Keep as a module when:**

| Signal | Example |
|--------|---------|
| Tightly coupled to parent | `runner.rs` is inseparable from `openings` |
| Shares private types | Internal state machines |
| Less than 500 lines | Probably doesn't warrant its own crate |
| No external consumers | Implementation details |

### Crate Naming Convention

```
runloop-{domain}         # Core libraries: runloop-kb, runloop-bus
runloop-{domain}-{sub}   # Sub-components: runloop-agent-registry
{binary-name}            # Binaries: runloopd, rlp, agtop
```

### Dependency Direction Rules

1. **Dependencies flow downward** — Lower layers never import higher layers
2. **Core has no internal dependencies** — Only external crates (serde, thiserror)
3. **Binaries depend on libraries** — Never library -> binary
4. **No circular dependencies** — If A needs B and B needs A, extract common to C
5. **Trait in lower layer, impl in higher** — e.g., `Executor` trait in openings,
   impl in runtime

### Async vs Sync Boundaries

```
+-------------------------------------------------------------+
|                    ASYNC BOUNDARY                           |
|  runloopd, rlp, agtop - tokio runtime                       |
|  Bus I/O, network calls, file system                        |
+-------------------------------------------------------------+
                          |
                          | spawn_blocking for:
                          v
+-------------------------------------------------------------+
|                    SYNC BOUNDARY                            |
|  SQLite operations (rusqlite is sync)                       |
|  BLAKE3 hashing (CPU-bound)                                 |
|  JSON Schema validation                                     |
|  YAML parsing                                               |
+-------------------------------------------------------------+
```

**Rules:**
- Public APIs that do I/O should be `async`
- CPU-bound work > 1ms should use `spawn_blocking`
- Never block the async runtime with sync I/O
- KB operations are sync internally, wrapped in `spawn_blocking` at call sites

### Module Organization Within a Crate

```
crates/kb/
├── Cargo.toml
├── src/
│   ├── lib.rs          # Public re-exports only (< 200 lines)
│   ├── error.rs        # Error types
│   ├── config.rs       # Configuration structs
│   ├── store.rs        # Core storage logic
│   ├── query.rs        # Query interface
│   ├── materialize.rs  # View materialization
│   └── verify.rs       # Integrity verification
└── tests/
    ├── integration.rs  # Cross-module tests
    └── fixtures/       # Test data
```

### Architectural Evolution Roadmap

These are planned architectural improvements. When working in related areas,
consider whether your changes can advance these goals.

#### KB Storage Abstraction

**Current state:** `runloop-kb` has SQLite implementation tightly coupled.

**Target state:** Split into two crates:
```
runloop-kb-core     # Traits, event types, query interface
runloop-kb-sqlite   # SQLite implementation (current code)
runloop-kb-postgres # Future: PostgreSQL for multi-node deployments
```

**Why:** Enables alternative storage backends without changing consumer code.
Multi-node deployments need shared storage; embedded deployments benefit from
SQLite.

**When to do it:** When adding a second storage backend, or when KB interface
changes require touching all consumers anyway.

#### Protocol Crate Extraction

**Current state:** Content type constants (`CT_*`) and message schemas live in
`runloop-core/src/content.rs`.

**Target state:** Extract to standalone `runloop-protocol` crate:
```
runloop-protocol    # Message schemas, content types, wire format
```

**Why:** Third-party tools (monitoring, debugging, custom agents) need to
parse Runloop messages without depending on full runtime.

**When to do it:** When external consumers request protocol access, or when
versioning the protocol independently becomes necessary.

#### WASM Agent SDK Consolidation

**Current state:** Each WASM agent duplicates FFI boilerplate:
```rust
// Repeated in 8+ agents
#[allow(unsafe_code)]
mod host {
    #[link(wasm_import_module = "runloop")]
    unsafe extern "C" {
        fn notify_ready();
    }
    pub fn signal_ready() { unsafe { notify_ready() }; }
}
```

**Target state:** Single implementation in `runloop-agent-wasm-sdk`:
```rust
// In SDK
pub fn signal_ready() {
    // SAFETY: Host runtime provides this function with no parameters.
    // Calling it cannot violate memory safety.
    unsafe { host::notify_ready() };
}

// In agents
use runloop_agent_wasm_sdk::signal_ready;
```

**Why:** Reduces duplication, ensures consistent safety comments, single point
of update for FFI changes.

**When to do it:** Next time any agent FFI code changes.

### Code Consolidation Guidelines

When you notice duplicated patterns, follow this decision process:

#### Step 1: Is it truly duplication?

```
Similar code in 2 places  → Probably coincidence, leave it
Similar code in 3+ places → Likely pattern, consider extracting
Identical code in 2+ places → Definitely extract
```

#### Step 2: Where should shared code live?

| Duplication Scope | Extract To |
|-------------------|------------|
| Within one crate | Private module in that crate |
| Across crates in same domain | Lowest common dependency |
| Across unrelated crates | `runloop-core` or new shared crate |
| Test utilities only | `tests/common/mod.rs` or test-utils crate |

#### Step 3: Environment Variable Helpers (Specific Example)

**Current state:** Unsafe env var manipulation scattered across:
- `crates/rlp/src/main.rs:639-645`
- `crates/rlp/src/shell.rs:738-754`
- `crates/runtime/src/secrets.rs:572-574`
- Multiple test files

**Target state:** Single helper in `runloop-core`:
```rust
// runloop-core/src/env.rs

use std::ffi::OsStr;

/// Temporarily sets an environment variable for the duration of a closure.
///
/// # Safety
///
/// Environment variable mutation is inherently unsafe in multi-threaded
/// programs. This function is safe to use when:
/// - Called from single-threaded test code, OR
/// - The variable is only read by code within the same closure
///
/// # Example
///
/// ```rust
/// with_env_var("API_KEY", Some("test-key"), || {
///     assert_eq!(std::env::var("API_KEY").unwrap(), "test-key");
/// });
/// ```
pub fn with_env_var<F, R>(key: &str, value: Option<&str>, f: F) -> R
where
    F: FnOnce() -> R,
{
    let previous = std::env::var_os(key);

    match value {
        Some(v) => {
            // SAFETY: Called in controlled context per function contract
            unsafe { std::env::set_var(key, v) };
        }
        None => {
            unsafe { std::env::remove_var(key) };
        }
    }

    let result = f();

    match previous {
        Some(v) => unsafe { std::env::set_var(key, v) },
        None => unsafe { std::env::remove_var(key) },
    }

    result
}
```

#### Step 4: Refactoring Checklist

Before extracting shared code:

- [ ] Identify all instances of the pattern (use `grep`/`rg`)
- [ ] Verify they're semantically identical, not just textually similar
- [ ] Choose appropriate location per table above
- [ ] Write the shared implementation with proper documentation
- [ ] Update all call sites in a single PR (or stacked PRs if large)
- [ ] Add tests for the shared code
- [ ] Remove any `#[allow(...)]` that become unnecessary

---

## 4. Code Size Limits

### Hard Limits

| Scope | Limit | Action if Exceeded |
|-------|-------|-------------------|
| **Function body** | 100 lines | Extract helper functions |
| **impl block** | 300 lines | Split into multiple impl blocks or extract types |
| **Module (single file)** | 800 lines | Split into submodules |
| **Crate lib.rs** | 200 lines | Move logic to submodules, keep lib.rs as re-exports |
| **Test module** | 500 lines | Split into multiple test files |

### Soft Limits (triggers review discussion)

| Scope | Limit | Rationale |
|-------|-------|-----------|
| **Function parameters** | 5 | Use struct/builder if more |
| **Match arms** | 10 | Consider lookup table or trait dispatch |
| **Nesting depth** | 4 levels | Extract to functions, use early returns |
| **Cyclomatic complexity** | 15 | Split into smaller functions |
| **Crate total size** | 5000 lines | Consider splitting domain |

### Measuring Code Size

```bash
# Lines per file (find largest)
find crates -name '*.rs' -exec wc -l {} + | sort -n | tail -20

# Function complexity
cargo clippy -- -W clippy::cognitive_complexity

# Check specific crate
tokei crates/kb/src
```

### When to Split

**Split a function when:**
- It has more than one level of abstraction
- You need to scroll to see it all
- The name doesn't fully describe what it does
- You find yourself adding comments to explain sections

**Split a module when:**
- It has multiple distinct responsibilities
- Tests are larger than the code they test
- Two developers frequently have merge conflicts
- You can draw a clear interface boundary

### Example: Splitting a Large Function

```rust
// BEFORE: 150-line function
pub async fn run(&self) -> Result<RunReport, RunnerError> {
    // 40 lines: Initialize state
    // 60 lines: Execute nodes
    // 30 lines: Handle failures
    // 20 lines: Build trace
}

// AFTER: Decomposed
pub async fn run(&self) -> Result<RunReport, RunnerError> {
    let mut state = self.initialize_state();

    while let Some(node_id) = state.next_ready_node() {
        self.execute_node(&mut state, &node_id).await?;
    }

    self.finalize_run(state)
}

fn initialize_state(&self) -> RunState { /* 40 lines */ }

async fn execute_node(
    &self,
    state: &mut RunState,
    node_id: &str,
) -> Result<(), RunnerError> { /* 60 lines */ }

fn finalize_run(&self, state: RunState) -> Result<RunReport, RunnerError> {
    /* 50 lines */
}
```

---

## 5. Rust Standards

### Safety Policy

```rust
// Workspace-level: deny by default
#![deny(unsafe_code)]

// Only allow when absolutely necessary (FFI, performance-critical paths)
#[allow(unsafe_code)]
mod ffi {
    // SAFETY: <explain why this is sound>
    // - What invariants must hold
    // - What could go wrong if they don't
    // - Why we believe they hold
    unsafe fn hostcall(...) { }
}
```

**Rules:**
- Every `unsafe` block MUST have a `// SAFETY:` comment directly above it
- Prefer safe abstractions even at slight performance cost
- When unsafe is required (WASM FFI, Pin projections), isolate in dedicated modules
- Get maintainer sign-off before adding new unsafe code

### Error Handling

Use `thiserror` for library errors:

```rust
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]  // Always add for public enums
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{context}: {source}")]
    Contextual {
        context: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}
```

**Never panic in library code:**

```rust
// FORBIDDEN in library crates:
.unwrap()
.expect("...")
panic!()
unreachable!()  // unless truly unreachable by type system

// Use instead:
.ok_or_else(|| Error::Missing("field"))?
.unwrap_or_default()
.map_or(fallback, |v| transform(v))
```

### Type System Leverage

**Use newtypes for domain concepts:**

```rust
// GOOD: Distinct types prevent mixing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceId(pub u128);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AgentId(pub Uuid);

// Can't accidentally pass TraceId where AgentId expected

// BAD: Raw primitives everywhere
fn process(trace_id: u128, agent_id: Uuid) { }  // Easy to swap arguments
```

**Encode states in types:**

```rust
// State machine in types
pub struct Connection<S: State> {
    inner: TcpStream,
    _state: PhantomData<S>,
}

pub struct Disconnected;
pub struct Connected;
pub struct Authenticated;

impl Connection<Disconnected> {
    pub fn connect(addr: &str) -> Result<Connection<Connected>, Error> { }
}

impl Connection<Connected> {
    pub fn authenticate(self, creds: &Creds) -> Result<Connection<Authenticated>, Error> { }
}

impl Connection<Authenticated> {
    pub fn send(&mut self, msg: &Message) -> Result<(), Error> { }
}
// Compile-time guarantee: can't send without authenticating
```

**Builder pattern for complex construction:**

```rust
pub struct RuntimeBuilder {
    sockets_dir: Option<PathBuf>,
    caps: CapabilitySet,
    audit_policy: AuditPolicy,
}

impl RuntimeBuilder {
    pub fn new() -> Self { Self::default() }

    #[must_use]
    pub fn sockets_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.sockets_dir = Some(path.into());
        self
    }

    #[must_use]
    pub fn capability(mut self, cap: Caps) -> Self {
        self.caps.add(cap);
        self
    }

    pub fn build(self) -> Result<Runtime, Error> {
        let sockets_dir = self.sockets_dir
            .ok_or(Error::Config("sockets_dir required"))?;
        // ...
    }
}
```

### Ownership and Borrowing

**Prefer borrowing over cloning:**

```rust
// GOOD: Borrow when you don't need ownership
fn process_message(msg: &Message) -> Result<(), Error> { }

// GOOD: Take ownership only when storing or sending across threads
fn spawn_handler(msg: Message) {
    tokio::spawn(async move { process(msg).await });
}

// BAD: Clone to satisfy borrow checker
fn bad_example(data: &Data) {
    let cloned = data.field.clone();  // Often unnecessary
    some_function(&cloned);
}
```

**Use `Cow` for flexible ownership:**

```rust
use std::borrow::Cow;

// Accept both owned and borrowed
fn log_error(message: impl Into<Cow<'static, str>>) {
    let msg: Cow<'static, str> = message.into();
    // No allocation for string literals
    // Owned strings work too
}

log_error("static string");           // No allocation
log_error(format!("dynamic: {}", x)); // Works with owned
```

**Prefer slices in function signatures:**

```rust
// GOOD: Accept slices, callers can pass Vec, array, or slice
fn process_items(items: &[Item]) { }

// GOOD: Accept &str, callers can pass String or &str
fn process_name(name: &str) { }

// BAD: Unnecessarily restrictive
fn bad_items(items: &Vec<Item>) { }  // Can't pass arrays
fn bad_name(name: &String) { }       // Can't pass &str
```

### Async and Concurrency

**Choose the right synchronization primitive:**

```rust
// For single-threaded async: prefer tokio primitives
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};

// For shared state across sync/async boundaries: parking_lot
use parking_lot::Mutex;  // Faster than std, no poisoning

// For concurrent maps with many readers: DashMap
use dashmap::DashMap;
```

**Decision tree:**
- Need async `.lock().await`? -> `tokio::sync::Mutex`
- Held across `.await`? -> `tokio::sync::Mutex`
- Short critical sections, sync code? -> `parking_lot::Mutex`
- Many readers, few writers? -> `DashMap` or `RwLock`

**Avoid holding locks across await points:**

```rust
// BAD: Lock held across await
async fn bad() {
    let guard = self.state.lock();
    do_async_work().await;  // Other tasks blocked!
    guard.update();
}

// GOOD: Minimize lock scope
async fn good() {
    let data = {
        let guard = self.state.lock();
        guard.data.clone()
    };
    let result = do_async_work(data).await;
    {
        let mut guard = self.state.lock();
        guard.update(result);
    }
}
```

**Use structured concurrency:**

```rust
// Parent task owns child tasks
async fn orchestrate() -> Result<(), Error> {
    let (results_tx, mut results_rx) = mpsc::channel(32);

    let workers: Vec<_> = (0..4)
        .map(|i| {
            let tx = results_tx.clone();
            tokio::spawn(async move { worker(i, tx).await })
        })
        .collect();

    drop(results_tx);  // Close sender so receiver terminates

    while let Some(result) = results_rx.recv().await {
        process_result(result)?;
    }

    // Wait for all workers, propagate errors
    for handle in workers {
        handle.await??;
    }

    Ok(())
}
```

---

## 6. Testing Standards

### Coverage Requirements

| Code Type | Minimum Coverage |
|-----------|-----------------|
| Core logic (runner, KB, bus) | 80% line coverage |
| Error paths | All error variants exercised |
| Public API | Every public function has at least one test |
| Unsafe code | 100% coverage + property tests |

### Test Organization

```rust
// Unit tests: same file, test module
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_input() { }

    #[test]
    fn rejects_invalid_input() { }
}

// Integration tests: tests/ directory
// crates/kb/tests/materialization.rs

// Property tests: for security-critical or parser code
#[cfg(test)]
mod proptest_tests {
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn never_panics(input in ".*") {
            let _ = parse(&input);  // Should not panic
        }
    }
}
```

### Test Naming Convention

```rust
#[test]
fn <unit>_<scenario>_<expected_behavior>() { }

// Examples:
fn router_shell_command_routes_to_shell() { }
fn kb_duplicate_hash_is_rejected() { }
fn bus_expired_message_emits_drop_notice() { }
```

### Mock External Dependencies

```rust
// Define trait for external dependency
#[async_trait]
pub trait SecretProvider: Send + Sync {
    async fn resolve(&self, key: &str) -> Result<String, Error>;
}

// Production implementation
pub struct EnvSecretProvider;

#[async_trait]
impl SecretProvider for EnvSecretProvider {
    async fn resolve(&self, key: &str) -> Result<String, Error> {
        std::env::var(key).map_err(|_| Error::SecretNotFound(key.into()))
    }
}

// Test mock
#[cfg(test)]
pub struct MockSecretProvider {
    secrets: HashMap<String, String>,
}

#[cfg(test)]
#[async_trait]
impl SecretProvider for MockSecretProvider {
    async fn resolve(&self, key: &str) -> Result<String, Error> {
        self.secrets.get(key).cloned()
            .ok_or_else(|| Error::SecretNotFound(key.into()))
    }
}
```

### Property-Based Testing

Use proptest for:
- Parsers (never panic on arbitrary input)
- Serialization (round-trip property)
- Security-critical code (router classification)
- Numeric operations (overflow handling)

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn roundtrip_serialization(msg: Message) {
        let bytes = serialize(&msg);
        let decoded = deserialize(&bytes).unwrap();
        prop_assert_eq!(msg, decoded);
    }

    #[test]
    fn classifier_never_panics(input in ".*") {
        let router = Router::default();
        let _ = router.classify(&input);
    }
}
```

---

## 7. Documentation Standards

### Every Public Item Must Be Documented

```rust
/// Knowledge base access layer backed by SQLite event logs.
///
/// The KB uses a dual-database architecture:
/// - `events.sqlite`: Append-only ledger of all events
/// - `pog.sqlite`: Materialized views for fast queries
///
/// # Example
///
/// ```rust
/// let kb = KnowledgeBase::open(&config)?;
/// kb.propose(delta)?;
/// let results = kb.query("SELECT * FROM contacts")?;
/// ```
///
/// # Thread Safety
///
/// `KnowledgeBase` is `Clone + Send + Sync`. Cloning shares the underlying
/// connection pools via `Arc`.
pub struct KnowledgeBase { }

/// Proposes a state change to the ledger.
///
/// # Errors
///
/// Returns [`Error::Schema`] if the payload fails JSON Schema validation.
/// Returns [`Error::MissingEvidence`] if referenced evidence IDs don't exist.
///
/// # Panics
///
/// This function does not panic.
pub fn propose(&self, delta: StateDelta) -> Result<EventId, Error> { }
```

### Module-Level Documentation

```rust
//! Message bus for inter-component communication.
//!
//! # Architecture
//!
//! The bus provides pub/sub messaging with:
//! - Topic-based routing
//! - Per-subscriber deduplication
//! - TTL enforcement
//! - Backpressure handling
//!
//! # Usage
//!
//! ```rust,no_run
//! let server = Bus::bind("/run/runloop/bus.sock").await?;
//! let client = Bus::connect("/run/runloop/bus.sock").await?;
//! let mut sub = client.subscribe("events/*").await?;
//! while let Some(msg) = sub.next().await {
//!     process(msg)?;
//! }
//! ```
```

### Documentation Sections

For complex functions, include:

- **Summary** (first line)
- **Extended description** (if needed)
- **# Arguments** (for non-obvious parameters)
- **# Returns** (if not obvious from signature)
- **# Errors** (list error conditions)
- **# Panics** (document or state "does not panic")
- **# Examples** (for public APIs)
- **# Safety** (for unsafe functions)

---

## 8. Performance Guidelines

### Avoid Allocations in Hot Paths

```rust
// BAD: Allocates on every call
fn format_key(prefix: &str, id: u64) -> String {
    format!("{}:{}", prefix, id)
}

// GOOD: Pre-allocate or use stack buffer
fn format_key_fast(prefix: &str, id: u64, buf: &mut String) {
    buf.clear();
    buf.push_str(prefix);
    buf.push(':');
    use std::fmt::Write;
    write!(buf, "{}", id).unwrap();
}

// GOOD: For small strings, use stack allocation
use arrayvec::ArrayString;
fn format_key_stack(prefix: &str, id: u64) -> ArrayString<64> {
    let mut buf = ArrayString::new();
    write!(&mut buf, "{}:{}", prefix, id).unwrap();
    buf
}
```

### Use Iterators Over Manual Loops

```rust
// GOOD: Lazy evaluation, often optimized to loops
let sum: u64 = items
    .iter()
    .filter(|item| item.active)
    .map(|item| item.value)
    .sum();

// GOOD: Collect only when needed
let active_ids: Vec<_> = items
    .iter()
    .filter_map(|item| item.active.then_some(item.id))
    .collect();
```

### Profile Before Optimizing

```rust
// Add benchmarks for performance-critical code
#[cfg(test)]
mod benchmarks {
    use criterion::{criterion_group, criterion_main, Criterion};

    fn bench_classify(c: &mut Criterion) {
        let router = Router::default();
        c.bench_function("classify_shell", |b| {
            b.iter(|| router.classify("ls -la"))
        });
    }

    criterion_group!(benches, bench_classify);
    criterion_main!(benches);
}
```

### Performance Review Checklist

Before merging performance-sensitive code:

- [ ] Benchmarks exist and show improvement
- [ ] No regressions in existing benchmarks
- [ ] Memory allocations measured (use `#[global_allocator]` with counting)
- [ ] Hot path identified via profiling, not guessing
- [ ] Before/after numbers in PR description

---

## 9. Dependency Management

### Before Adding a Dependency

Answer these questions:

1. **Is it necessary?** Can we achieve this with std or existing deps?
2. **Is it maintained?** Last release < 1 year, issues triaged
3. **Is it small?** Prefer focused crates over kitchen-sink dependencies
4. **Is it safe?** Run `cargo audit`, check for known vulnerabilities
5. **License compatible?** Apache-2.0, MIT, BSD-3-Clause are safe

**Approval required for:**
- Any crate with `unsafe` in public API
- Crates with > 50 transitive dependencies
- Crates that add new system capabilities (network, filesystem, FFI)

### Preferred Crates

| Purpose | Recommended | Avoid |
|---------|-------------|-------|
| Errors | `thiserror` | `failure`, `error-chain` |
| Async runtime | `tokio` | `async-std` (for consistency) |
| Serialization | `serde` + `serde_json` | hand-rolled |
| Hashing | `blake3` | `sha2` (for non-crypto), `md5` |
| CLI | `clap` | `structopt` (merged into clap) |
| HTTP client | `reqwest` | `hyper` (unless low-level needed) |
| Concurrency | `parking_lot`, `dashmap` | `crossbeam` (unless channels) |
| Regex | `regex` | `pcre2`, `fancy-regex` (unless needed) |
| UUID | `uuid` | `ulid` (unless ordering needed) |

### Updating Dependencies

```bash
# Check for outdated deps
cargo outdated

# Check for security vulnerabilities
cargo audit

# Update conservatively (patch versions only)
cargo update

# Update specific crate to new minor/major
cargo update -p <crate>
```

---

## 10. Logging and Observability

### Structured Logging

Use `tracing` for all logging with structured fields:

```rust
use tracing::{info, warn, error, instrument, Span};

// GOOD: Structured fields for machine parsing
info!(
    trace_id = %trace_id,
    agent_id = %agent_id,
    duration_ms = elapsed.as_millis(),
    "agent execution completed"
);

// BAD: Interpolated strings lose structure
info!("agent {} completed in {}ms", agent_id, elapsed.as_millis());
```

### Span Context

Always propagate trace context through async boundaries:

```rust
#[instrument(skip(self), fields(trace_id = %self.trace_id))]
pub async fn execute(&self, node_id: &str) -> Result<(), Error> {
    // All logs within this function automatically include trace_id
    info!(node_id, "starting node execution");

    let result = self.run_node(node_id).await;

    match &result {
        Ok(_) => info!(node_id, "node succeeded"),
        Err(e) => warn!(node_id, error = %e, "node failed"),
    }

    result
}
```

### Log Levels

| Level | Use For | Example |
|-------|---------|---------|
| `error` | Unrecoverable failures, data loss risk | DB write failed, capability violation |
| `warn` | Recoverable issues, degraded operation | Retry succeeded, rate limited |
| `info` | Significant state changes | Agent started, opening completed |
| `debug` | Detailed flow for troubleshooting | Message received, cache hit |
| `trace` | Very verbose, hot path details | Per-byte parsing, lock acquired |

### Metrics

Emit metrics for operational visibility:

```rust
use metrics::{counter, gauge, histogram};

// Counters for events
counter!("runloop.openings.completed", "status" => "success").increment(1);
counter!("runloop.openings.completed", "status" => "failed").increment(1);

// Gauges for current state
gauge!("runloop.agents.active").set(active_count as f64);

// Histograms for distributions
histogram!("runloop.node.duration_ms").record(elapsed.as_millis() as f64);
```

### Error Context

Always add context when propagating errors across boundaries:

```rust
use tracing::error;

pub async fn handle_request(&self, req: Request) -> Result<Response, Error> {
    let trace_id = req.trace_id;

    self.process(req)
        .await
        .map_err(|e| {
            // Log at boundary with full context
            error!(
                trace_id = %trace_id,
                error = %e,
                "request processing failed"
            );
            e
        })
}
```

### What NOT to Log

- **Secrets**: API keys, tokens, passwords (redact or omit)
- **PII**: Email addresses, names (use IDs or redact)
- **Large payloads**: Full message bodies (log size/hash instead)
- **High-frequency events at info level**: Use debug/trace

```rust
// BAD: Logs secret
info!(api_key = %key, "calling external API");

// GOOD: Logs presence, not value
info!(api_key_present = !key.is_empty(), "calling external API");

// BAD: Logs PII
info!(email = %user.email, "sending notification");

// GOOD: Logs user ID
info!(user_id = %user.id, "sending notification");
```

---

## 11. API Design Guidelines

### Public API Surface Minimization

Only expose what's necessary:

```rust
// lib.rs - Explicit, minimal re-exports
pub use config::Config;
pub use error::Error;
pub use store::{KnowledgeBase, QueryResult};

// Internal modules not re-exported
mod cache;
mod materialize;
mod schema;
```

### Required Attributes for Public Types

```rust
// Enums that may grow
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NodeState {
    Pending,
    Running,
    Succeeded,
    Failed { reason: String },
    Skipped,
}

// Structs with important return values
#[must_use = "RunReport contains the execution trace"]
#[derive(Debug, Clone)]
pub struct RunReport {
    pub trace: RunTrace,
    pub node_records: Vec<NodeRecord>,
}

// Builder methods
impl RuntimeBuilder {
    #[must_use]
    pub fn with_capability(mut self, cap: Capability) -> Self {
        self.caps.push(cap);
        self
    }
}
```

### Generics vs Trait Objects

**Use generics (static dispatch) when:**

```rust
// Compile-time known types, zero-cost abstraction
pub fn serialize<T: Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

// impl Trait for ergonomic return types
pub fn active_items(&self) -> impl Iterator<Item = &Item> {
    self.items.iter().filter(|i| i.active)
}
```

**Use trait objects (dynamic dispatch) when:**

```rust
// Runtime polymorphism needed (plugins, user-provided impls)
pub struct Runtime {
    secret_provider: Box<dyn SecretProvider>,
    executors: Vec<Box<dyn Executor>>,
}

// Heterogeneous collections
pub fn register_handler(&mut self, handler: Box<dyn Handler>) {
    self.handlers.push(handler);
}
```

### Input Validation at Boundaries

Validate all external input at system boundaries:

```rust
/// Validates and parses an opening YAML from untrusted input.
///
/// # Security
///
/// This function is a trust boundary. Input is assumed adversarial.
pub fn parse_opening_str(yaml: &str) -> Result<Opening, Error> {
    // 1. Size limit to prevent DoS
    if yaml.len() > MAX_OPENING_SIZE {
        return Err(Error::Validation {
            message: "opening exceeds size limit".into(),
            location: None,
        });
    }

    // 2. Parse YAML (may fail on malformed input)
    let raw: RawOpening = serde_yaml::from_str(yaml)
        .map_err(|e| Error::Parse(e.to_string()))?;

    // 3. Validate schema
    validate_opening_schema(&raw)?;

    // 4. Validate semantic constraints
    validate_no_cycles(&raw)?;
    validate_port_references(&raw)?;

    // 5. Return validated, trusted type
    Ok(Opening::from_raw(raw))
}
```

### Extension Points

Design for extension without modification:

```rust
// Trait for user-provided implementations
#[async_trait]
pub trait Executor: Send + Sync {
    async fn execute(&self, request: NodeExecutionRequest<'_>)
        -> Result<NodeExecution, RunnerError>;

    // Default implementations for optional methods
    fn name(&self) -> &str {
        "unnamed"
    }

    fn supports_retry(&self) -> bool {
        true
    }
}

// Registry pattern for dynamic registration
pub struct ExecutorRegistry {
    executors: HashMap<String, Arc<dyn Executor>>,
}

impl ExecutorRegistry {
    pub fn register(&mut self, name: impl Into<String>, executor: Arc<dyn Executor>) {
        self.executors.insert(name.into(), executor);
    }
}
```

### Backward Compatibility

When evolving APIs:

```rust
// 1. Add new fields with defaults
#[derive(Debug, Deserialize)]
pub struct Config {
    pub required_field: String,
    #[serde(default)]
    pub new_optional_field: Option<String>,  // Added in v0.2
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,  // Added in v0.3 with default
}

// 2. Deprecate before removing
#[deprecated(since = "0.4.0", note = "use `new_method` instead")]
pub fn old_method(&self) { }

// 3. Use #[non_exhaustive] to allow adding enum variants
#[non_exhaustive]
pub enum Event {
    Started,
    Completed,
    Failed,
    // Can add new variants without breaking downstream matches
}
```

---

## 12. Code Review Checklist

Before submitting a PR, verify against this checklist:

### Safety

- [ ] No new `unsafe` blocks without maintainer approval
- [ ] All `unsafe` blocks have `// SAFETY:` comments explaining:
  - What invariants must hold
  - Why we believe they hold
  - What could go wrong if they don't
- [ ] No `.unwrap()` or `.expect()` in library code
- [ ] No `panic!()` or `unreachable!()` without type-system proof
- [ ] Input validated at system boundaries

### Correctness

- [ ] Error cases return `Err`, not panic
- [ ] Resources properly cleaned up (RAII patterns, Drop impls)
- [ ] No data races (verify with types, consider `cargo +nightly miri test`)
- [ ] Async code doesn't hold locks across `.await`
- [ ] State machines have valid transitions only

### Types

- [ ] Newtypes used for domain concepts (IDs, not raw primitives)
- [ ] `#[non_exhaustive]` on public enums that may grow
- [ ] `#[must_use]` on functions/types where ignoring result is likely a bug
- [ ] `Cow<str>` considered for strings that may be static or owned

### Style

- [ ] `cargo fmt --all` passes
- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] Functions under 100 lines
- [ ] Modules under 800 lines
- [ ] Nesting depth under 4 levels
- [ ] No TODO/FIXME without linked issue

### Documentation

- [ ] All public items have `///` doc comments
- [ ] Complex functions document `# Errors` and `# Panics`
- [ ] Module has `//!` header explaining purpose
- [ ] Examples compile (`cargo test --doc`)

### Tests

- [ ] New functionality has tests
- [ ] Error paths exercised
- [ ] Property tests for parsers/security-critical code
- [ ] Existing tests still pass

### Performance (if applicable)

- [ ] No obvious N+1 patterns
- [ ] No allocations in hot loops
- [ ] Benchmarks exist for critical paths
- [ ] Before/after numbers in PR description

---

## 13. Git and PR Workflow

### Branch Naming

```
<type>/<issue-number>-<short-description>

feat/42-add-replay-verification
fix/87-kb-deadlock-on-backup
docs/103-update-api-reference
refactor/115-extract-validation
```

### Commit Messages

Follow Conventional Commits:

```
<type>(<scope>): <short description>

<body explaining what and why, not how>

<footer with references>
```

**Types:** `feat`, `fix`, `docs`, `refactor`, `test`, `perf`, `chore`, `ci`

**Examples:**

```
feat(router): add case-insensitive command matching

The router now matches commands like "LS" and "Git" correctly.
Previously, only lowercase commands were recognized as shell routes.

Closes #42
```

```
fix(kb): prevent deadlock on concurrent materialization

The materializer was holding a read lock while requesting a write lock,
causing deadlock under concurrent load. Fixed by releasing the read lock
before acquiring write lock.

Fixes #87
```

### Commit Hygiene

- **Atomic commits**: Each commit should be a single logical change
- **Buildable**: Every commit should pass `cargo build` and `cargo test`
- **Signed**: Include `Signed-off-by:` for DCO compliance

```bash
# Add DCO sign-off
git commit -s -m "feat(bus): add message deduplication"

# Amend if you forgot
git commit --amend -s
```

### PR Size Guidelines

| Size | Lines Changed | Review Time |
|------|---------------|-------------|
| Small | < 100 | Same day |
| Medium | 100-400 | 1-2 days |
| Large | 400-800 | 2-3 days |
| Too Large | > 800 | Split the PR |

**Splitting large PRs:**

1. **Refactor first**: Extract/move code in one PR, add feature in next
2. **Feature flags**: Merge incomplete feature behind flag
3. **Vertical slices**: Complete one use case end-to-end per PR
4. **Stacked PRs**: Chain dependent PRs with clear base branches

### PR Description Template

```markdown
## Summary

Brief description of what this PR does.

## Changes

- Bullet points of specific changes
- Include file:line references for key changes

## Testing

- How was this tested?
- Any manual verification steps?

## Checklist

- [ ] Self-reviewed the diff
- [ ] Added/updated tests
- [ ] Updated documentation
- [ ] Ran `just pre-commit` locally

## Related

Closes #123
Related to #456
```

### Review Etiquette

**As author:**
- Respond to all comments before requesting re-review
- Don't resolve comments yourself (let reviewer resolve)
- Keep PR updated with main (rebase preferred over merge)

**As reviewer:**
- Use conventional comment prefixes:
  - `nit:` — Minor style suggestion, non-blocking
  - `question:` — Seeking clarification
  - `suggestion:` — Proposed alternative
  - `issue:` — Must be addressed before merge
- Approve with comments for nits, request changes for issues

---

## 14. Exemplary Patterns in the Codebase

These are examples of well-done code in Runloop that demonstrate the standards
in this document. Use them as reference when writing similar code.

### RAII with Commit/Rollback (Bus Deduplication)

Location: `crates/bus/src/lib.rs`

The bus deduplication uses RAII to ensure cleanup on both success and failure:

```rust
struct DedupeReservation {
    cache: Arc<Mutex<DedupeCache>>,
    key: (u128, u64),
    committed: bool,
}

impl DedupeReservation {
    /// Mark the reservation as successfully processed.
    fn commit(&mut self) {
        self.committed = true;
    }

    /// Explicitly roll back (remove from cache).
    fn rollback(&mut self) {
        if self.committed {
            return;
        }
        if let Ok(mut cache) = self.cache.lock() {
            cache.remove(self.key);
        }
        self.committed = true;
    }
}

impl Drop for DedupeReservation {
    fn drop(&mut self) {
        // Auto-rollback if not committed
        if !self.committed {
            self.rollback();
        }
    }
}
```

**Why it's exemplary:**
- Automatic cleanup on drop (can't forget to rollback)
- Explicit commit for success path
- Handles partial failures gracefully
- Lock is released even on panic

### Property-Based Testing (Router Classifier)

Location: `crates/router/src/classifier.rs`

The router uses proptest to verify it never panics on arbitrary input:

```rust
#[cfg(test)]
mod proptest_tests {
    use proptest::prelude::*;

    proptest! {
        /// The classifier should never panic on any arbitrary Unicode string.
        #[test]
        fn classifier_never_panics_on_arbitrary_input(input in ".*") {
            let router = router_default();
            let classification = router.classify(&input);
            prop_assert!(matches!(classification.route, Route::Shell | Route::Agent));
            prop_assert!(!classification.rule.is_empty());
        }

        /// The classifier should handle strings with embedded null bytes.
        #[test]
        fn classifier_handles_embedded_nulls(
            prefix in "[a-z]{0,10}",
            suffix in "[a-z]{0,10}"
        ) {
            let router = router_default();
            let input = format!("{}\0{}", prefix, suffix);
            let classification = router.classify(&input);
            prop_assert!(matches!(classification.route, Route::Shell | Route::Agent));
        }
    }
}
```

**Why it's exemplary:**
- Tests security-critical code with arbitrary input
- Covers edge cases humans wouldn't think of
- Documents expected invariants (never panics)
- Uses specific generators for targeted testing

### Integrity Verification (Knowledge Base)

Location: `crates/kb/src/lib.rs`

The KB verification recomputes hashes to detect corruption:

```rust
pub fn verify(&self) -> Result<VerifyReport, Error> {
    let mut report = VerifyReport::default();

    for event in self.iter_events()? {
        // Recompute hash from canonical JSON
        let canonical = serde_json::to_vec(&event.payload)?;
        let computed_hash = blake3::hash(&canonical);

        if computed_hash != event.stored_hash {
            report.corrupted.push(CorruptedEvent {
                event_id: event.id,
                expected: event.stored_hash,
                computed: computed_hash,
            });
        }

        // Verify chain integrity
        if let Some(parent) = event.parent_id {
            if !self.event_exists(parent)? {
                report.orphaned.push(event.id);
            }
        }

        report.verified_count += 1;
    }

    Ok(report)
}
```

**Why it's exemplary:**
- Post-hoc audit capability for compliance
- Uses canonical serialization for reproducibility
- Reports all issues rather than failing on first
- Structured report enables automated processing

### Structured Error Types (Runtime)

Location: `crates/runtime/src/error.rs`

Capability denials carry structured context:

```rust
/// Structured capability denial context propagated to callers.
#[derive(Debug, Clone)]
pub struct CapDeniedInfo {
    pub cap: CapKind,
    pub op: String,
    pub detail: String,
    pub reason: String,
    pub audit_event: Option<EventId>,
}

impl CapDeniedInfo {
    #[must_use]
    pub fn new(
        cap: impl AsRef<str>,
        op: impl Into<String>,
        detail: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            cap: CapKind::from_label(cap.as_ref()),
            op: op.into(),
            detail: detail.into(),
            reason: reason.into(),
            audit_event: None,
        }
    }
}
```

**Why it's exemplary:**
- Typed capability kinds enable matching
- All context needed for debugging included
- Links to audit log for forensics
- Ergonomic constructors with `impl Into<String>`

### Mock Executor for Testing (Openings)

Location: `crates/openings/src/lib.rs`

The opening tests use a mock executor for deterministic testing:

```rust
struct MockExecutor {
    responses: Mutex<HashMap<String, NodeExecution>>,
}

impl MockExecutor {
    fn new(map: HashMap<String, NodeExecution>) -> Self {
        Self {
            responses: Mutex::new(map),
        }
    }
}

#[async_trait]
impl Executor for MockExecutor {
    async fn execute(
        &self,
        request: NodeExecutionRequest<'_>,
    ) -> Result<NodeExecution, RunnerError> {
        let guard = self.responses.lock().expect("mock executor poisoned");
        guard
            .get(&request.node.id)
            .cloned()
            .ok_or_else(|| RunnerError::Executor(format!("no mock for {}", request.node.id)))
    }
}
```

**Why it's exemplary:**
- Tests opening logic without real agent execution
- Configurable responses per node
- Error on missing mock (catches test bugs)
- Simple, focused implementation

---

## 15. Anti-Patterns to Avoid

### The "Flexible Framework" Trap

```rust
// BAD: Over-engineered for hypothetical future
trait Processor<I, O, E, C> {
    fn process(&self, input: I, ctx: C) -> Result<O, E>;
}

// GOOD: Solve today's problem
fn process_message(msg: &Message) -> Result<Response, Error> { }
```

### The "Just In Case" Clone

```rust
// BAD: Cloning to avoid thinking about lifetimes
fn process(data: &Data) {
    let owned = data.items.clone(); // "just in case"
    for item in &owned { /* ... */ }
}

// GOOD: Use references when possible
fn process(data: &Data) {
    for item in &data.items { /* ... */ }
}
```

### The "God Module"

```rust
// BAD: Everything in one place
// lib.rs with 3000 lines, 50 public functions

// GOOD: Focused modules with clear boundaries
mod parse;    // ~200 lines
mod validate; // ~150 lines
mod execute;  // ~300 lines
mod report;   // ~100 lines
```

### The "Silent Failure"

```rust
// BAD: Error swallowed
fn try_send(msg: &Message) {
    let _ = sender.send(msg.clone()); // Ignores failure
}

// GOOD: Handle or propagate
fn try_send(msg: &Message) -> Result<(), SendError> {
    sender.send(msg.clone()).map_err(|e| {
        tracing::warn!(?e, "send failed, will retry");
        SendError::ChannelFull
    })
}
```

### The "Stringly Typed" API

```rust
// BAD: Strings for everything
fn create_agent(kind: &str, mode: &str) -> Result<Agent, Error> {
    match kind {
        "writer" => { },
        "critic" => { },
        _ => return Err(Error::InvalidKind),
    }
}

// GOOD: Enums for fixed sets
enum AgentKind { Writer, Critic }
enum Mode { Development, Production }

fn create_agent(kind: AgentKind, mode: Mode) -> Result<Agent, Error> {
    // Compile-time guarantee of valid values
}
```

### The "Primitive Obsession"

```rust
// BAD: Raw types that can be confused
fn schedule(
    timeout_ms: u64,
    retry_count: u64,
    delay_ms: u64,
) { }

// Easy to call with arguments in wrong order
schedule(3, 1000, 5);  // Oops! timeout and delay swapped

// GOOD: Newtypes prevent confusion
struct TimeoutMs(u64);
struct RetryCount(u32);
struct DelayMs(u64);

fn schedule(timeout: TimeoutMs, retries: RetryCount, delay: DelayMs) { }

// Compile error if order is wrong
schedule(TimeoutMs(1000), RetryCount(3), DelayMs(5));
```

### The "Async Infection"

```rust
// BAD: Making everything async "just in case"
async fn parse_config(path: &Path) -> Result<Config, Error> {
    let content = tokio::fs::read_to_string(path).await?;
    // Rest is pure computation, no I/O
    let config: Config = toml::from_str(&content)?;
    validate(&config)?;
    Ok(config)
}

// GOOD: Sync where possible, async only for I/O
fn parse_config_content(content: &str) -> Result<Config, Error> {
    let config: Config = toml::from_str(content)?;
    validate(&config)?;
    Ok(config)
}

// Async wrapper only when needed
async fn load_config(path: &Path) -> Result<Config, Error> {
    let content = tokio::fs::read_to_string(path).await?;
    parse_config_content(&content)
}
```

### The "Error String" Pattern

```rust
// BAD: Errors as strings lose type information
fn validate(input: &str) -> Result<(), String> {
    if input.is_empty() {
        return Err("input is empty".to_string());
    }
    if input.len() > 100 {
        return Err(format!("input too long: {} chars", input.len()));
    }
    Ok(())
}

// GOOD: Typed errors enable matching and recovery
#[derive(Debug, Error)]
enum ValidationError {
    #[error("input is empty")]
    Empty,
    #[error("input too long: {len} chars (max {max})")]
    TooLong { len: usize, max: usize },
}

fn validate(input: &str) -> Result<(), ValidationError> {
    if input.is_empty() {
        return Err(ValidationError::Empty);
    }
    if input.len() > 100 {
        return Err(ValidationError::TooLong { len: input.len(), max: 100 });
    }
    Ok(())
}
```

---

## 16. Quick Reference Card

```
+-------------------------------------------------------------+
|                 RUNLOOP RUST GUIDELINES                     |
+-------------------------------------------------------------+
| SAFETY                                                      |
|   X  unsafe without // SAFETY comment                       |
|   X  .unwrap() / .expect() in library code                  |
|   X  panic!() / unreachable!() without proof                |
|   OK #[deny(unsafe_code)] at crate level                    |
|   OK validate at boundaries, trust internally               |
+-------------------------------------------------------------+
| ERRORS                                                      |
|   OK thiserror for library errors                           |
|   OK #[non_exhaustive] on public enums                      |
|   OK From implementations for crate boundaries              |
|   OK anyhow only in binaries                                |
+-------------------------------------------------------------+
| TYPES                                                       |
|   OK Newtypes for domain concepts (TraceId, AgentId)        |
|   OK Builder pattern for complex construction               |
|   OK #[must_use] on types/functions with important returns  |
|   OK Cow<str> for flexible string ownership                 |
+-------------------------------------------------------------+
| SIZE LIMITS                                                 |
|   Function body:    100 lines max                           |
|   impl block:       300 lines max                           |
|   Module file:      800 lines max                           |
|   lib.rs:           200 lines max (re-exports only)         |
|   Function params:  5 max (use struct if more)              |
|   Nesting depth:    4 levels max                            |
+-------------------------------------------------------------+
| ASYNC                                                       |
|   OK tokio::sync for async-aware locks                      |
|   OK parking_lot for sync-only short critical sections      |
|   X  holding locks across .await                            |
|   OK structured concurrency (parent owns children)          |
+-------------------------------------------------------------+
| TESTS                                                       |
|   OK 80%+ coverage for core logic                           |
|   OK proptest for parsers and security-critical code        |
|   OK mock traits for external dependencies                  |
|   OK integration tests in tests/ directory                  |
+-------------------------------------------------------------+
| DECISIONS (in priority order)                               |
|   1. Reliability - does it fail gracefully?                 |
|   2. Security - is it safe against malicious input?         |
|   3. Debuggability - can I diagnose issues from logs?       |
|   4. Maintainability - can others understand it?            |
|   5. Performance - only after 1-4 are satisfied             |
+-------------------------------------------------------------+
| PRE-COMMIT                                                  |
|   $ cargo fmt --all                                         |
|   $ cargo clippy --workspace -- -D warnings                 |
|   $ cargo test --workspace                                  |
|   $ cargo doc --no-deps                                     |
+-------------------------------------------------------------+
```

---

*Last updated: 2025-01*
