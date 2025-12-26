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

6. Build the wasm agent bundles and run the smoke test:

   ```bash
   just build-agents-wasm
   just test-agents-wasm
   ```

   These helpers compile the wasm32-wasip1 binaries under `agents/*/bin/` and
   exercise `compose_email` end-to-end via `rlp --local`.

## Common operational commands

- `rlp kb migrate|verify|backup|vacuum`

> Secrets/trust CLIs are still planned: `rlp secrets put|get|list|delete` and
> `rlp trust update` remain interface contracts until their implementations land
> with the packaging milestone. `rlp agent install` is available for local
> bundles (directory or .tar/.tar.gz) and performs digest/tools.json validation;
> `rlp agent remove` is still pending. Use `rlp agent list` to inspect bundles
> already present in your search dirs.

Finer details appear in `docs/ops.md`. As implementation lands, these commands
gain real outputs; until then they serve as interface contracts.

## Installing from the Debian package

1. Build the `.deb` from source (Debian 13/trixie host):

   ```bash
   just deb
   ```

   Install `cargo-deb` first (`cargo install cargo-deb`). Artifacts land under
   each crate’s `target/debian/` directory, e.g.
   `crates/runloopd/target/debian/runloopd_<version>_<arch>.deb`.

2. Install and start the daemon:

   ```bash
   sudo apt install crates/runloopd/target/debian/runloopd_0.1.0_amd64.deb
   sudo systemctl status runloopd
   ```

   Install the CLI/TUI helpers separately if desired:

   ```bash
   sudo apt install crates/rlp/target/debian/rlp_0.1.0_amd64.deb
   sudo apt install crates/agtop/target/debian/agtop_0.1.0_amd64.deb
   ```

   The daemon package writes the default config to `/etc/runloop/config.yaml`,
   creates `/var/lib/runloop`, and enables (but does not automatically start)
   the `runloopd` systemd unit so you can edit config before running it.

3. Update `/etc/runloop/config.yaml` as needed, then run
   `sudo systemctl restart runloopd`. Purging the package removes the config,
   state, and the `runloop` system user.

## Authoring an agent on a Debian install

If you install Runloop from the packages and want to add your own agent without
cloning the source tree, follow `docs/authoring-on-deb.md` for the scaffold →
edit → build → run loop (Rust + wasm32-wasip1 target required).

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
