# Release Process

This document replaces ad-hoc release notes with a repeatable checklist. It
covers semantic versioning, approvals, and the artifacts we ship for each tag.

## Versioning & Tags

- We follow SemVer. While the project is pre-1.0, use `v0.<minor>.<patch>`.
  Increment `<minor>` for roadmap milestones (Phase-level features) and
  `<patch>` for fixes. The first stable GA will be tagged `v1.0.0` per the
  roadmap.
- Tags live on `main`. Cut a release branch only when preparing a beta/RC that
  needs additional soak; otherwise cherry-pick critical fixes directly onto the
  tag and create `v0.x.y+1`.
- Annotate every tag (`git tag -a v0.x.y -m "Runloop v0.x.y"`) so `git
  describe` remains meaningful for debugging.

## Changelog Rules

- CHANGELOG entries are generated from Conventional Commits. Before tagging,
  aggregate commits since the previous release (e.g.,
  `git log --format='%s' <prev-tag>..HEAD`) and group them by `feat`, `fix`,
  `perf`, `docs`, `chore`, etc.
- Manually summarize any noteworthy highlights (perf wins, security fixes) at
  the top of the release section. Include links to key PRs.
- If a change requires operator action (schema migration, new config flag), add
  an **Action Required** bullet with the exact commands/config snippets.

## Release Approvals

- Every release requires sign-off from: (1) the release manager for the phase,
  (2) a runtime or platform engineer covering `runloopd`/`runtime`, and (3) the
  security reviewer listed in `SECURITY.md` when capabilities or signing
  changes are included.
- Before tagging, confirm CI is green on `main` and the following commands pass
  locally in release mode: `cargo build --workspace --release`, `cargo test
  --workspace`, `cargo clippy --workspace -- -D warnings`, and the perf harness
  documented in [docs/perf.md](perf.md).
- Capture the manual verification steps (smoke tests, upgrade/downgrade, sample
  opening replay) in the release PR description; attach terminal output or TUI
  screenshots for traceability.

## Artifacts

- **Crates & Binaries:** Publish the `runloopd`, `rlp`, and `agtop` binaries to
  the GitHub release along with SHA256 sums.
- **Debian packages:** Build via `packaging/systemd/` using `just deb`. Verify
  install on Debian 12 (x86_64 and arm64) and ensure `runloopd.service` starts.
- **Containers/dev images:** Build from `packaging/container/` for CI and
  developer environments. Tag images with the exact git SHA.
- **Live ISO:** Optional for minor releases; mandatory for public betas. The
  scripts live under `packaging/live-build/`.
- Document where each artifact is uploaded (GitHub Releases, package repo,
  internal mirror) and include download instructions in the release notes.

## Verification Checklist

1. `cargo fmt --all`, `cargo clippy --workspace -- -D warnings`, `cargo test
   --workspace`
2. `cargo build --workspace --release`
3. Performance harness: `cargo test -p runloop-runtime cold_start_p50_under_40ms`
   and `cargo bench -p runloop-runtime cold_start` on the baseline hardware.
4. Security review: confirm signed agent bundles install and that capability
   denials emit KB audit records.
5. Docs: `mdbook build docs` and update `CHANGELOG.md`, `docs/index.md`, any
   relevant specs (policy, openings, perf budgets) to reflect the release.
6. Tag and push: `git tag -a v0.x.y -m "Runloop v0.x.y" && git push origin
   v0.x.y`.

Record the completion timestamp and signer in the release issue for auditing.
