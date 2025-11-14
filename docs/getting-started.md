# Getting Started (Draft)

> **Doc status:** Informative — links to normative specs. Last updated:
> 2025-11-02.

## Prerequisites

- Debian/Ubuntu host (or container/VM) with Rust toolchain (`rustup`, `cargo`,
  `clippy`, `rustfmt`).
- Optional tooling: `just`, `sqlite3`, `cargo-deb`, `live-build`,
  `qemu-system-x86_64`.
- `rlp` CLI (will be built from source once crates exist).

## Repository layout refresher

See the tree in `README.md` for directories. Key docs:

- `docs/message-protocol.md` — wire format spec (normative)
- `docs/rmp-registry.md` — schema IDs
- `docs/kb-schemas.md` — ledger & materialized views (normative)
- `docs/ops.md` — operations, config precedence, trust policy (normative
  sections marked)
- `docs/security-model.md` — sandbox, secret store, threat model

## First-run checklist

1. Clone repo and read `README.md` front-to-back.
2. Configure `~/.runloop/config.yaml` (copy from `.env.example` guidance when
   available).
3. Initialize secrets backend (optional):

   ```bash
   rlp secrets init --backend=secret-service
   ```

4. Initialize KB once binaries land:

   ```bash
   rlp kb migrate   # creates events.sqlite, pog.sqlite, vectors/
   rlp kb verify
   ```

5. Review trust policy (after Release key published):

   ```bash
   cat ~/.runloop/trust-policy.toml
   ```

## Common operational commands

- `rlp kb migrate|verify|backup|vacuum`

> Secrets/trust/agent CLIs are still planned: `rlp secrets put|get|list|delete`,
> `rlp trust update`, and `rlp agent install|list|remove` remain interface
> contracts until their implementations land with the packaging milestone.

Finer details appear in `docs/ops.md`. As implementation lands, these commands
gain real outputs; until then they serve as interface contracts.

## Finding work items

- `TODO.md` → repo scaffolding checklist
- `ROADMAP.md` → milestone-level goals
- Issues/Discussions (once public) will tag bugs/features/tasks

## Contributing flow (preview)

1. Fork/branch following `CONTRIBUTING.md` guidance (to be fleshed out).
2. For spec changes, open an ADR (`docs/adr/`) and mark affected docs as
   Draft/Normative.
3. Ensure docs and fixtures stay in sync (e.g., schema registry, KB migrations).

Questions? Open an issue or reach out via channels in `SUPPORT.md`.
