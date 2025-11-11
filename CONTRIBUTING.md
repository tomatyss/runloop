# Contributing to Runloop

## Workflow & branches

- We use trunk-based development. Create a short-lived branch off `main`, open a
  PR, get review, and merge.
- Branch names: `feat/<slug>`, `fix/<slug>`, `docs/<slug>`, `chore/<slug>`.

## Project scope & expectations

- Status: pre-alpha; expect specs and APIs to move quickly.
- Focus: terminal-first runtime, agent SDK, and knowledge base—stay within the
  roadmap phases unless coordinating with maintainers.
- Coordination: open an issue before large refactors or new features; align
  proposals with ROADMAP milestones.

## Issues & labels

- Use our templates. Labels: bug, feature, task, docs, infra, security, design,
  good-first-issue, epic, phase:g.

## Commit style

- Conventional Commits required. Examples:
  - feat(router): classify shell vs agent with explainability
  - fix(kb): guard null payload in materializer
- CHANGELOG is generated from
  `feat|fix|perf|refactor|docs|chore|ci|build|revert`.

## Lint & tests (required to merge)

- Rust: `cargo fmt --all` and `cargo clippy --workspace -- -D warnings`
- Tests: `cargo test --workspace`
- Docs: markdownlint, link check
- Commits: DCO Signed-off-by required
- Pre-commit: `just pre-commit` runs the Rust format, clippy, and test gates
  locally; symlink it with
  `ln -s ../../scripts/pre-commit.sh .git/hooks/pre-commit` to enforce on every
  commit.

## DCO

By contributing, you agree to the Developer Certificate of Origin (DCO 1.1). Add
a trailer to each commit: `Signed-off-by: Your Name <you@example.com>`

## Code review

- At least 1 reviewer from CODEOWNERS for owned paths
- CI must be green
