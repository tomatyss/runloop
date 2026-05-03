# Runloop Agent Guide

This file is the short, always-loaded map for coding agents working on Runloop.
Keep detailed standards in docs and update this file only for rules that should
be active on every task.

For full technical standards, see
[docs/engineering-standards.md](docs/engineering-standards.md). For
Codex-specific workflow, see [docs/codex-workflow.md](docs/codex-workflow.md).

## Continuity Ledger

Maintain a single continuity ledger in `CONTINUITY.md`.

- At the start of every assistant turn, read `CONTINUITY.md`, update it for the
  latest goal, constraints, decisions, state, and next step, then proceed.
- Update it again whenever the goal, assumptions, decisions, progress, or
  important tool outcomes change.
- Keep it short: stable facts only, no transcript. Mark uncertainty as
  `UNCONFIRMED`.
- Begin user replies with a brief "Ledger Snapshot" containing Goal, Now/Next,
  and Open Questions.

Use `functions.update_plan` for short-lived execution steps. Use `CONTINUITY.md`
for durable context that must survive compaction.

## Project Map

```text
crates/
  core/              shared types, IDs, config
  rmp/               message envelope encoding
  bus/               pub/sub and IPC
  kb/                SQLite knowledge base and trace storage
  runtime/           WASM runtime, hostcalls, capabilities
  openings/          workflow DSL parser and runner
  router/            shell-vs-agent prompt classification
  agent-registry/    agent manifest discovery and bundle validation
  model-broker/      LLM provider abstraction
  executor-local/    local executor wiring runtime, KB, broker, agents
  runloopd/          daemon and control plane
  rlp/               CLI
  agtop/             TUI monitor
  agents/            native agent implementations
  agents-wasm/       WASM agent implementations, mostly outside root workspace
docs/                architecture, standards, mdbook
agents/              agent manifests
examples/            sample openings
packaging/           Debian and systemd assets
```

Dependency direction matters: shared crates (`core`, `rmp`) sit below bus, KB,
runtime, openings, executors, and binaries. Libraries must not depend on
binaries. Avoid circular dependencies; extract shared contracts downward.

## Build And Test

Prefer the `Justfile` targets when available:

```bash
just fmt
just clippy
just test
just all
```

Equivalent raw commands:

```bash
cargo fmt --all
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo check --workspace
```

Targeted checks:

```bash
cargo test -p <crate>
cargo test -p runloop-executor-local --test golden -- --ignored
just build-agents-wasm
just test-agents-wasm
```

Use the narrowest check that validates the change while iterating, then run the
broader checks before finishing when the change touches shared behavior,
runtime/capabilities, daemon control flow, parser/runner behavior, or public CLI
surfaces.

## Done Criteria

Before handing work back:

- Explain what changed and why.
- Run relevant formatting, linting, and tests, or state why they were not run.
- Review your own diff for regressions, security issues, and accidental churn.
- Leave unrelated user changes untouched.
- Keep `CONTINUITY.md` current.

## Rust Standards

Use the detailed rules in
[docs/engineering-standards.md](docs/engineering-standards.md). The high-signal
rules are:

- Reliability first, then security, debuggability, maintainability, performance.
- No `.unwrap()`, `.expect()`, `panic!()`, or `unreachable!()` in library code
  unless the invariant is type-proven and documented. Tests may use them.
- Library errors use `thiserror`; public enums that may grow use
  `#[non_exhaustive]`.
- Use newtypes for domain IDs and fixed concepts. Avoid stringly typed APIs.
- Prefer borrowed inputs (`&str`, slices) unless ownership is required.
- Do not hold locks across `.await`; minimize lock scope.
- Use structured `tracing` fields. Do not log secrets, PII, or large payloads.
- Public APIs should be small, documented, and exported intentionally from
  `lib.rs`.
- Add tests for new behavior and for relevant error paths.

## Security-Sensitive Areas

Be conservative in these paths and run broader checks:

- `crates/runtime/src/hostcalls.rs`
- `crates/runtime/src/caps.rs`
- `crates/runtime/src/secrets.rs`
- `crates/bus/`
- `crates/kb/`
- `crates/runloopd/src/control.rs`
- `crates/runloopd/src/engine.rs`
- `crates/agent-registry/`
- `crates/rlp/src/agent.rs`

For these areas, explicitly consider capability enforcement, path traversal,
secret exposure, message spoofing, replay/idempotency, and audit visibility.

## Workflow Expectations

- Use `rg`/`rg --files` for code search.
- Read nearby code and existing tests before editing.
- Keep changes scoped to the requested behavior; do not bundle unrelated
  refactors.
- Use `apply_patch` for manual edits.
- If work is complex, ambiguous, or likely to span multiple milestones, create
  or update an execution plan as described in
  [docs/codex-workflow.md](docs/codex-workflow.md).
- For code review requests, lead with findings ordered by severity, with
  file/line references. If there are no findings, say so and name residual test
  gaps.

## Git And PR

Follow [CONTRIBUTING.md](CONTRIBUTING.md) and the PR template.

- Commit messages use `<type>(<scope>): <description>`.
- Include a `Signed-off-by` trailer on commits.
- Do not rewrite, revert, or clean up user changes unless explicitly asked.
- Before a PR or handoff, report tests run and any ignored/skipped coverage.

## Keeping This Useful

When an agent makes the same mistake twice, update this file if the rule should
apply globally. If the rule is detailed, put it in docs and link to it here.
