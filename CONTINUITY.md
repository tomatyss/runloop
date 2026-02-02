Goal (incl. success criteria):
- Run `just pre-commit`, fix any issues it reports, and leave the repo passing the pre-commit checks.

Constraints/Assumptions:
- Follow AGENTS.md ledger updates each turn.
- Use repo tooling (`just pre-commit`).
- Avoid destructive git actions and keep changes scoped to fixes.

Key decisions:
- Capture `strict_fs_caps` before thread spawn to avoid borrowing `self` across thread boundary.

State:
- Done: `just pre-commit` passes after fixes; status/diff checked.
- Now: Await user direction (commit or further changes).
- Next: Commit if requested.

Done:
- Updated `crates/runtime/src/runtime.rs` to move `strict_fs_caps` into the thread context.
- Updated `docs/ops.md` to wrap a long line (MD013).
- Ran `just pre-commit` successfully.
- Checked `git status` and `git diff --stat`.

Now:
- Await user direction.

Next:
- Commit changes if requested.

Open questions (UNCONFIRMED if needed):
- None.

Working set (files/ids/commands):
- crates/runtime/src/runtime.rs
- docs/ops.md
- CONTINUITY.md
- `git status -sb`
- `git diff --stat`
