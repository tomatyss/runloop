# Debian (systemd) Packaging

This directory houses the Debian 13 (trixie) packaging assets for Runloop:

- `runloopd.service` – systemd unit running the daemon as `runloop:runloop` with
  sockets rooted at `/run/runloop`.
- `tmpfiles.d/runloop.conf` – runtime and state directories materialized via
  `systemd-tmpfiles`.
- `config/config.yaml` – default system-mode configuration installed to
  `/etc/runloop/config.yaml`.
- `README.Debian` – admin notes, shipped under `/usr/share/doc/runloop/`.
- `debian/` – control files copied to the repository root when building the
  package (control, rules, maintainer scripts, etc.).
- `build-deb.sh` – helper that syncs `debian/` into place, runs
  `dpkg-buildpackage`, and cleans up.

## Building the package

Prerequisites on Debian 13:

```bash
sudo apt install build-essential debhelper dh-cargo rustc cargo pkg-config \
                 libssl-dev libsqlite3-dev systemd systemd-dev
```

Then, from the repo root:

```bash
just deb
# or directly:
packaging/systemd/build-deb.sh
```

Artifacts land in the parent directory (e.g.,
`../runloop_0.1.0~alpha1-1_amd64.deb`).

## Installing

```bash
sudo apt install ./../runloop_0.1.0~alpha1-1_amd64.deb
sudo systemctl status runloopd
```

The daemon runs in the background, persists data under `/var/lib/runloop`, and
listens on `/run/runloop/rmp.sock`. Update `/etc/runloop/config.yaml` and
restart the service when changing models, capabilities, or socket locations.
