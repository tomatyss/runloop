# Repository Guidelines for AI Agents

This document provides comprehensive instructions for AI coding agents working on
Runloop OS. Follow these guidelines to maintain code quality and consistency.

For the full technical standards with all examples, see
[docs/engineering-standards.md](docs/engineering-standards.md).

---

## Table of Contents

1. [Project Structure](#project-structure)
2. [Architecture Overview](#architecture-overview)
3. [Build and Test Commands](#build-and-test-commands)
4. [Code Style Rules](#code-style-rules)
5. [Error Handling](#error-handling)
6. [Type System Rules](#type-system-rules)
7. [Ownership and Borrowing](#ownership-and-borrowing)
8. [Async and Concurrency](#async-and-concurrency)
9. [Testing Requirements](#testing-requirements)
10. [Logging and Observability](#logging-and-observability)
11. [API Design](#api-design)
12. [Architectural Decisions](#architectural-decisions)
13. [Anti-Patterns to Avoid](#anti-patterns-to-avoid)
14. [Commit and PR Guidelines](#commit-and-pr-guidelines)
15. [Quick Reference Card](#quick-reference-card)

---

## Project Structure

```text
runloop-os/
├── crates/
│   ├── runloopd/          # Main daemon
│   ├── rlp/               # CLI tool
│   ├── agtop/             # TUI monitoring
│   ├── core/              # Shared types, IDs, config
│   ├── bus/               # Message bus (pub/sub)
│   ├── kb/                # Knowledge base (SQLite)
│   ├── runtime/           # WASM runtime
│   ├── openings/          # Workflow DSL
│   ├── router/            # Prompt classification
│   ├── agent-registry/    # Agent discovery
│   ├── model-broker/      # LLM abstraction
│   └── agents-wasm/       # WASM agent implementations
├── docs/                  # Documentation
├── packaging/             # Debian/systemd files
├── examples/              # Sample openings
└── agents/                # Agent manifests
```

---

## Architecture Overview

### Crate Dependency Diagram

```text
                 runloop-core (shared types)
                        │
        ┌───────────────┼───────────────┐
        │               │               │
        ▼               ▼               ▼
   runloop-bus    runloop-kb     runloop-rmp
        │               │
        └───────┬───────┘
                │
                ▼
        runloop-runtime
                │
                ▼
        runloop-openings
                │
        ┌───────┼───────┐
        │       │       │
        ▼       ▼       ▼
    runloopd   rlp    agtop  (binaries)
```

### Dependency Rules

1. **Dependencies flow downward only** — Never import from higher layers
2. **Core has no internal deps** — Only external crates (serde, thiserror)
3. **Binaries at leaf** — Libraries never depend on binaries
4. **No circular deps** — Extract common code if A needs B and B needs A

### When to Create a New Crate

| Create New Crate When        | Keep as Module When       |
| ---------------------------- | ------------------------- |
| Different dependency tree    | Tightly coupled to parent |
| Reusable by external tools   | Shares private types      |
| Different lint requirements  | Less than 500 lines       |
| Clear domain boundary        | No external consumers     |

---

## Build and Test Commands

```bash
# Format (required before commit)
cargo fmt --all

# Lint (must pass with no warnings)
cargo clippy --workspace -- -D warnings

# Build
cargo build --workspace
cargo build --workspace --release  # For packaging

# Test
cargo test --workspace
cargo test -p <crate>              # Single crate
cargo test -p runloop-executor-local --test golden -- --ignored  # Golden tests

# Documentation
cargo doc --no-deps --open
```

---

## Code Style Rules

### Naming Conventions

| Item                   | Convention              | Example           |
| ---------------------- | ----------------------- | ----------------- |
| Crates, modules, files | `snake_case`            | `agent_registry`  |
| Types, traits          | `UpperCamelCase`        | `AgentRegistry`   |
| Functions, variables   | `snake_case`            | `resolve_agent`   |
| Constants              | `SCREAMING_SNAKE_CASE`  | `MAX_RETRY_COUNT` |

### Size Limits (Hard)

| Scope           | Limit     | Action                |
| --------------- | --------- | --------------------- |
| Function body   | 100 lines | Extract helpers       |
| impl block      | 300 lines | Split or extract      |
| Module file     | 800 lines | Split into submodules |
| lib.rs          | 200 lines | Re-exports only       |
| Function params | 5         | Use struct/builder    |
| Nesting depth   | 4 levels  | Extract, early return |

---

## Error Handling

### Use `thiserror` for Library Errors

```rust
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]  // ALWAYS add this
pub enum Error {
    #[error("config error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
```

### NEVER in Library Code

```rust
// FORBIDDEN:
.unwrap()
.expect("...")
panic!()
unreachable!()  // unless proven by types

// USE INSTEAD:
.ok_or_else(|| Error::Missing("field"))?
.unwrap_or_default()
.map_or(fallback, |v| transform(v))
```

### Error Propagation

```rust
// HANDLE when you can recover
fn retry_on_network_error() {
    match fetch() {
        Err(Error::Network(_)) => retry(),
        other => other,
    }
}

// PROPAGATE when caller has more context
fn library_function() -> Result<T, Error> {
    inner_call()?  // Let caller decide
}

// LOG AND PROPAGATE at boundaries
async fn handle_request() -> Result<Response, Error> {
    process().await.map_err(|e| {
        tracing::error!(error = %e, "request failed");
        e
    })
}
```

---

## Type System Rules

### Use Newtypes for Domain Concepts

```rust
// GOOD: Distinct types prevent mixing arguments
pub struct TraceId(pub u128);
pub struct AgentId(pub Uuid);

fn process(trace: TraceId, agent: AgentId) { }

// BAD: Easy to swap arguments
fn process(trace_id: u128, agent_id: Uuid) { }
```

### Prefer Slices and Borrowed Types

```rust
// GOOD: Accepts Vec, array, or slice
fn process_items(items: &[Item]) { }

// GOOD: Accepts String or &str
fn process_name(name: &str) { }

// BAD: Too restrictive
fn process_items(items: &Vec<Item>) { }
fn process_name(name: &String) { }
```

### Use `Cow` for Flexible Ownership

```rust
use std::borrow::Cow;

fn log_error(msg: impl Into<Cow<'static, str>>) {
    let msg: Cow<'static, str> = msg.into();
    // No allocation for "static string"
    // Works with format!("dynamic {}", x) too
}
```

### Required Attributes

```rust
// Public enums that may grow
#[non_exhaustive]
pub enum NodeState { Running, Done }

// Important return values
#[must_use = "RunReport contains the trace"]
pub struct RunReport { }

// Builder methods
#[must_use]
pub fn with_timeout(mut self, t: Duration) -> Self { }
```

---

## Ownership and Borrowing

### Clone vs Borrow Decision

| Clone When                   | Borrow When                  |
| ---------------------------- | ---------------------------- |
| Data is small (< 1KB)        | Data is large                |
| Called infrequently          | Called in hot loop           |
| Need owned for `Send`        | No ownership transfer needed |
| Simplifies API significantly | References work fine         |

### `Arc<Mutex<T>>` Pattern

```rust
// Use for shared mutable state across tasks
struct Service {
    state: Arc<Mutex<State>>,
}

// Minimize lock scope
let data = {
    let guard = self.state.lock();
    guard.data.clone()
};
// Lock released here
process(data).await;
```

---

## Async and Concurrency

### Choosing Sync Primitives

| Primitive              | Use When                   |
| ---------------------- | -------------------------- |
| `tokio::sync::Mutex`   | Lock held across `.await`  |
| `parking_lot::Mutex`   | Short sync-only sections   |
| `DashMap`              | Many readers, few writers  |
| `tokio::sync::RwLock`  | Read-heavy async access    |

### NEVER Hold Locks Across Await

```rust
// BAD: Blocks other tasks
async fn bad() {
    let guard = self.state.lock();
    do_io().await;  // Lock held during I/O!
    guard.update();
}

// GOOD: Release before await
async fn good() {
    let data = {
        let guard = self.state.lock();
        guard.data.clone()
    };  // Lock released
    let result = do_io().await;
    self.state.lock().update(result);
}
```

### Async vs Sync Boundaries

```text
ASYNC: I/O operations (network, file, bus)
SYNC:  CPU-bound work (hashing, parsing, validation)

Use spawn_blocking for sync work > 1ms
```

---

## Testing Requirements

### Coverage Requirements

| Code Type   | Minimum                 |
| ----------- | ----------------------- |
| Core logic  | 80% line coverage       |
| Error paths | All variants exercised  |
| Public API  | Every function tested   |
| Unsafe code | 100% + property tests   |

### Test Organization

```rust
// Unit tests: same file
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_input() { }
}

// Integration tests: tests/ directory
// Property tests: for parsers and security code
proptest! {
    #[test]
    fn never_panics(input in ".*") {
        let _ = parse(&input);
    }
}
```

### Test Naming

```rust
fn <unit>_<scenario>_<expected>() { }

// Examples:
fn router_shell_command_routes_to_shell() { }
fn kb_duplicate_hash_is_rejected() { }
```

---

## Logging and Observability

### Use Structured Logging

```rust
use tracing::{info, warn, error};

// GOOD: Structured fields
info!(
    trace_id = %trace_id,
    duration_ms = elapsed.as_millis(),
    "operation completed"
);

// BAD: String interpolation
info!("operation {} completed in {}ms", trace_id, elapsed);
```

### Log Levels

| Level   | Use For                   |
| ------- | ------------------------- |
| `error` | Unrecoverable failures    |
| `warn`  | Recoverable issues        |
| `info`  | Significant state changes |
| `debug` | Troubleshooting detail    |
| `trace` | Hot path verbosity        |

### What NOT to Log

- Secrets (API keys, tokens)
- PII (emails, names) — use IDs instead
- Large payloads — log size/hash instead

---

## API Design

### Public API Minimization

```rust
// lib.rs — Only re-export what's public
pub use config::Config;
pub use error::Error;
pub use store::KnowledgeBase;

// Keep internal modules private
mod cache;
mod internal;
```

### Generics vs Trait Objects

```rust
// GENERICS: Compile-time known, zero-cost
fn serialize<T: Serialize>(v: &T) -> Vec<u8> { }

// TRAIT OBJECTS: Runtime polymorphism, plugins
struct Runtime {
    executor: Box<dyn Executor>,
}
```

### Input Validation at Boundaries

```rust
/// Parse untrusted input — this is a trust boundary.
pub fn parse_opening(yaml: &str) -> Result<Opening, Error> {
    // 1. Size limit (DoS prevention)
    if yaml.len() > MAX_SIZE { return Err(...) }

    // 2. Parse
    let raw: RawOpening = serde_yaml::from_str(yaml)?;

    // 3. Validate schema
    validate_schema(&raw)?;

    // 4. Return trusted type
    Ok(Opening::from_raw(raw))
}
```

---

## Architectural Decisions

### Planned Refactorings

When working in these areas, consider advancing these goals:

#### KB Storage Abstraction

- Current: SQLite tightly coupled in `runloop-kb`
- Target: Split to `runloop-kb-core` + `runloop-kb-sqlite`
- Why: Enable PostgreSQL for multi-node deployments

#### Protocol Crate Extraction

- Current: Content types in `runloop-core/src/content.rs`
- Target: Standalone `runloop-protocol` crate
- Why: Third-party tools need protocol without full runtime

#### WASM Agent SDK Consolidation

- Current: FFI boilerplate duplicated in 8+ agents
- Target: Single impl in `runloop-agent-wasm-sdk`
- Why: Reduce duplication, consistent safety comments

### Code Duplication Guidelines

```text
2 places  → Probably coincidence, leave it
3+ places → Consider extracting
Identical → Definitely extract
```

| Duplication Scope         | Extract To                     |
| ------------------------- | ------------------------------ |
| Within one crate          | Private module                 |
| Across related crates     | Lowest common dependency       |
| Across unrelated crates   | `runloop-core` or new crate    |
| Test utilities            | `tests/common/mod.rs`          |

---

## Anti-Patterns to Avoid

### 1. The "Flexible Framework" Trap

```rust
// BAD: Over-engineered
trait Processor<I, O, E, C> {
    fn process(&self, input: I, ctx: C) -> Result<O, E>;
}

// GOOD: Solve today's problem
fn process_message(msg: &Message) -> Result<Response, Error> { }
```

### 2. The "Just In Case" Clone

```rust
// BAD
let owned = data.items.clone();  // "just in case"
for item in &owned { }

// GOOD
for item in &data.items { }
```

### 3. The "God Module"

```rust
// BAD: lib.rs with 3000 lines

// GOOD: Focused modules
mod parse;     // ~200 lines
mod validate;  // ~150 lines
mod execute;   // ~300 lines
```

### 4. The "Silent Failure"

```rust
// BAD: Error swallowed
let _ = sender.send(msg);

// GOOD: Handle or propagate
sender.send(msg).map_err(|e| {
    warn!(?e, "send failed");
    SendError::ChannelFull
})?;
```

### 5. The "Stringly Typed" API

```rust
// BAD
fn create(kind: &str, mode: &str) { }

// GOOD
enum Kind { Writer, Critic }
fn create(kind: Kind, mode: Mode) { }
```

### 6. The "Primitive Obsession"

```rust
// BAD: Easy to swap args
fn schedule(timeout_ms: u64, delay_ms: u64) { }

// GOOD: Newtypes
struct TimeoutMs(u64);
struct DelayMs(u64);
fn schedule(timeout: TimeoutMs, delay: DelayMs) { }
```

### 7. The "Async Infection"

```rust
// BAD: Async for pure computation
async fn parse(s: &str) -> Config {
    toml::from_str(s)  // No I/O!
}

// GOOD: Sync for computation
fn parse(s: &str) -> Config { toml::from_str(s) }
```

### 8. The "Error String"

```rust
// BAD
fn validate(s: &str) -> Result<(), String> { }

// GOOD
#[derive(Error)]
enum ValidationError {
    #[error("empty input")]
    Empty,
}
fn validate(s: &str) -> Result<(), ValidationError> { }
```

---

## Commit and PR Guidelines

### Commit Messages

```text
<type>(<scope>): <description>

<body>

<footer>
```

Types: `feat`, `fix`, `docs`, `refactor`, `test`, `perf`, `chore`

```text
feat(router): add case-insensitive matching

Closes #42
Signed-off-by: Name <email>
```

### PR Size

| Size      | Lines   | Action          |
| --------- | ------- | --------------- |
| Small     | < 100   | Same day review |
| Medium    | 100-400 | 1-2 days        |
| Large     | 400-800 | 2-3 days        |
| Too Large | > 800   | Split the PR    |

---

## Quick Reference Card

```text
+---------------------------------------------------------------+
|                    RUNLOOP AGENT RULES                        |
+---------------------------------------------------------------+
| FORBIDDEN                                                     |
|   .unwrap() / .expect() in library code                       |
|   panic!() / unreachable!() without type proof                |
|   unsafe without // SAFETY: comment                           |
|   Holding locks across .await                                 |
|   String for fixed enum values                                |
|   Raw primitives for domain IDs                               |
+---------------------------------------------------------------+
| REQUIRED                                                      |
|   #[non_exhaustive] on public enums                           |
|   #[must_use] on builders and important returns               |
|   /// doc comments on all public items                        |
|   // SAFETY: comment on every unsafe block                    |
|   Tests for new functionality                                 |
|   Signed-off-by on commits                                    |
+---------------------------------------------------------------+
| SIZE LIMITS                                                   |
|   Function: 100 lines    Module: 800 lines                    |
|   impl: 300 lines        lib.rs: 200 lines                    |
|   Params: 5 max          Nesting: 4 levels                    |
+---------------------------------------------------------------+
| DECISION PRIORITY                                             |
|   1. Reliability    2. Security    3. Debuggability           |
|   4. Maintainability    5. Performance                        |
+---------------------------------------------------------------+
| PRE-COMMIT                                                    |
|   cargo fmt --all                                             |
|   cargo clippy --workspace -- -D warnings                     |
|   cargo test --workspace                                      |
+---------------------------------------------------------------+
```
