# Debian (systemd) Packaging

This directory houses the Debian 13 (trixie) packaging assets for Runloop:

- `runloopd.service` – systemd unit running the daemon as `runloop:runloop` with
  sockets rooted at `/run/runloop`.
- `tmpfiles.d/runloop.conf` – runtime and state directories materialized via
  `systemd-tmpfiles`.
- `config/config.yaml` – default system-mode configuration installed to
  `/etc/runloop/config.yaml` (marked as a conffile).
- `README.Debian` – admin notes, shipped under `/usr/share/doc/runloopd/`.
- `scripts/*.{postinst,prerm,postrm}` – maintainer scripts referenced directly
  from the `cargo-deb` metadata.

There is no `debian/` subtree anymore; `cargo-deb` drives the entire build.

## Building the package

Prerequisites on Debian 13:

```bash
sudo apt install build-essential rustc cargo pkg-config libssl-dev libsqlite3-dev systemd
cargo install cargo-deb
```

Then, from the repo root:

```bash
just deb
# or to build an individual crate:
just build-agents-wasm
cargo deb -p runloopd
cargo deb -p rlp
cargo deb -p agtop
```

Artifacts land in each crate’s `target/debian/` directory. Copy them to your APT
staging area or `dpkg -i` directly.

## Installing

```bash
sudo apt install crates/runloopd/target/debian/runloopd_<version>_amd64.deb
sudo systemctl status runloopd
```

The daemon runs in the background, persists data under `/var/lib/runloop`, and
listens on `/run/runloop/rmp.sock`. Update `/etc/runloop/config.yaml` and
restart the service when changing models, capabilities, or socket locations.
Default agent bundles ship under `/usr/lib/runloop/agents` and the
`compose_email` and `smoke_exec` openings ship to
`/etc/runloop/openings/compose_email.yaml` and
`/etc/runloop/openings/smoke_exec.yaml`. The postinst script seeds writable
copies into `/var/lib/runloop/{agents,openings}` and refreshes them on upgrade
only if the seeded directories are unchanged (tracked via a directory hash).

## Release checklist

- Bump the `crates/rlp` version when the CLI surface changes (e.g., new agent
  subcommands) so `.deb` upgrades pick up the new binary.
- After `cargo deb -p rlp`, install the package and confirm `rlp --version` and
  `rlp agent --help` show the agent commands before publishing artifacts.
- Confirm `/usr/bin/rlp` is owned by the `rlp` package (not the `runloop`
  meta-package) to avoid shipping stale binaries.
