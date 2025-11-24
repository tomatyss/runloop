# Repository Guidelines

## Project Structure & Module Organization

Runloop OS is a Rust workspace rooted at `Cargo.toml`; individual services and
tooling live under `crates/` (e.g., `crates/runloopd` for the daemon,
`crates/rlp` for the CLI, `crates/agtop` for observability). Shared
documentation sits in `docs/`, with normative specs called out in the README.
Agent bundles and their capability manifests are kept in `agents/`, sample
scenarios in `examples/`, and deployment scaffolding in `packaging/` and
`infra/`. Tooling artifacts should stay under `target/` or a crate-local
`tests/` directory.

## Build, Test, and Development Commands

Use `cargo fmt --all` to enforce workspace formatting, then
`cargo clippy --workspace -- -D warnings` to satisfy lint gates before opening a
pull request. Build every crate with `cargo build --workspace`; add `--release`
when validating packaging artifacts. Run unit and integration suites via
`cargo test --workspace`, and execute targeted suites with
`cargo test -p <crate>` when iterating quickly.

## Coding Style & Naming Conventions

Follow rustfmt defaults (4-space indentation, LF line endings) and keep Markdown
at 2-space indents per `.editorconfig`. New crates, modules, and files should
use `snake_case`; public types remain `UpperCamelCase`, while constants use
`SCREAMING_SNAKE_CASE`. Avoid `unsafe` blocks unless coordinated with
maintainers—the workspace `deny` lint will flag them. Run `cargo doc --open`
locally when adding public APIs to confirm docs render and examples compile.

## Testing Guidelines

Module-level unit tests belong alongside source (`src/**/*.rs`) using
`#[cfg(test)]`; cross-crate behaviors should land in `tests/` directories to
exercise the WASM runtime and message bus together. Include fixtures for agent
contracts in `examples/openings/` so they double as documentation. For end-to-end
regression of openings, verify changes against the golden corpus using
`cargo test -p runloop-executor-local --test golden -- --ignored`. When touching
capability or policy code, add regression coverage around failure paths and
verify deterministic replay notes in `docs/ops.md` remain accurate.

## Commit & Pull Request Guidelines

Adopt Conventional Commits (`feat`, `fix`, `docs`, etc.) as enforced in
`CONTRIBUTING.md`, and append the required `Signed-off-by:` trailer (DCO 1.1).
Each PR should describe scope, linked issues, and manual verification steps;
attach terminal captures when behavior changes in `rlp` or `agtop`. Expect
CODEOWNERS review on owned paths and keep CI green (`fmt`, `clippy`, `test`,
markdown lint) before requesting approval.

## Security & Configuration Tips

Capability manifests in `agents/` must reflect any new filesystem, network, or
model access—update `docs/policy-caps.md` in tandem. Configuration defaults live
in `docs/configuration`; if you add a new flag, document it and provide a safe
fallback. Never commit secrets; rely on host env vars or `.env.example`
placeholders and confirm packaging scripts under `packaging/` still strip
sensitive data.
