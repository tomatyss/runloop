# Contributor Guide

This guide captures the day-to-day expectations that supplement
`CONTRIBUTING.md` and the workflows documented in `AGENTS.md`. Treat it as the
single source of truth for onboarding new collaborators.

## Picking an Issue

- Start with the GitHub issue tracker and filter by `good-first-issue` or
  roadmap labels (`epic`, `phase:*`) to find scoped work. Larger epics reference
  the milestones in [ROADMAP](roadmap.md).
- Please comment `/assign` (or leave a note) before beginning work so we avoid
  duplicate efforts. If an item touches multiple crates, outline the plan in the
  issue or create an Architecture Decision Record (ADR) stub.
- For work that spans more than a few days, draft a short proposal describing
  problem, approach, and acceptance criteria. Link it from the issue for review
  before coding.
- When you open new issues, include reproduction steps, expected vs. actual
  behavior, and whether the bug blocks a roadmap milestone.

## Style Conventions

- Rust code follows `cargo fmt --all` output and
  `cargo clippy --workspace -- -D warnings`. CI enforces both gates before
  merge.
- Naming: crates/files use `snake_case`, public types use `UpperCamelCase`, and
  constants use `SCREAMING_SNAKE_CASE`. Avoid `unsafe` blocks unless the
  maintainers have signed off; `deny(unsafe_code)` is enabled in the workspace.
- Markdown files use 2-space indentation per `.editorconfig`; keep lines under
  ~100 characters when practical.
- Tests live next to the modules they cover (`src/**/mod.rs` with
  `#[cfg(test)]`) and cross-crate behavior belongs in `tests/` harnesses. When a
  change alters capabilities or policies, add regression tests around the
  failure paths.
- **Golden Tests:** For end-to-end regression testing of openings, we maintain a
  golden corpus in `tests/golden/`. Run these tests using
  `cargo test --package runloop-executor-local --test golden -- --ignored`.
  These tests verify that openings produce expected outcomes (like correct
  recipient resolution or draft properties) across various input scenarios.
- Update documentation and examples alongside code changes. At minimum, touch
  `docs/` specs, `examples/openings/`, and `AGENTS.md` when you add new
  capabilities, commands, or bundles.

## Review Process

1. Create a short-lived branch from `main` (`feat/<slug>`, `fix/<slug>`,
   `docs/<slug>`, or `chore/<slug>`). Keep commits focused and use Conventional
   Commit prefixes; include `Signed-off-by` trailers for DCO 1.1 compliance.
2. Run `cargo fmt --all`, `cargo clippy --workspace -- -D warnings`, and
   `cargo test --workspace` locally. For docs-only changes, run `just docs-book`
   or `mdbook build docs` to catch link issues.
3. Open a PR that explains the motivation, links the relevant issue/roadmap
   item, and lists manual verification steps (screenshots for `rlp`/`agtop`
   output are highly encouraged).
4. Request review from the CODEOWNERS responsible for the touched paths. At
   least one CODEOWNER approval plus a green CI run are mandatory before merge.
5. Squash-or-merge via GitHub once approvals land; avoid force-pushing onto
   `main`. Release managers will cherry-pick fixes as needed for stabilization
   branches.

## Communication Norms

- Use GitHub Issues or Discussions for asynchronous design questions. Maintainer
  response-time goal is two US business days; if you need a faster answer, note
  the urgency in the issue title.
- Status updates: when you are assigned to an issue, post a short progress note
  at least every other day. If you are blocked, explain what is needed so a
  maintainer can help.
- Synchronous syncs (pairing, design walkthroughs) are opt-in. Ask for one in
  the issue/PR and propose time slots; maintainers will provide a video/voice
  link if necessary.
- Security-sensitive reports (e.g., capability bypasses) should go to the
  contact listed in `SECURITY.md` rather than a public issue.
