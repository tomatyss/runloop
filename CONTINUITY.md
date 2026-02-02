Goal (incl. success criteria):
- Review recent changes (postinst + docs) for issues and commit the git diff with proper message/sign-off.

Constraints/Assumptions:
- Follow AGENTS.md ledger updates each turn.
- Sandbox: workspace-write; network restricted.
- Review should prioritize correctness, data-loss risks, regressions, missing tests.

Key decisions:
- Directory-hash based seeding for agent bundles in postinst.
- Docs updated to reflect directory-hash tracking.

State:
- Done: Implemented directory hash seeding + conservative legacy handling; updated docs.
- Now: Perform code review of changes and commit.
- Next: Provide review findings summary and commit details.

Done:
- Updated `packaging/systemd/scripts/runloopd/postinst` with directory hashing and safe legacy seed handling.
- Updated docs: `README.md`, `packaging/systemd/README.md`, `packaging/systemd/README.Debian`, `docs/ops.md`.

Now:
- Review changes for risks/bugs.

Next:
- Commit the changes with Signed-off-by.

Open questions (UNCONFIRMED if needed):
- None.

Working set (files/ids/commands):
- packaging/systemd/scripts/runloopd/postinst
- README.md
- packaging/systemd/README.md
- packaging/systemd/README.Debian
- docs/ops.md
- CONTINUITY.md
