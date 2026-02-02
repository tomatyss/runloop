# Debian Test Container

This container is meant to validate the Debian packaging flow end-to-end:

- build the wasm bundles
- build the .deb packages
- install them inside a Debian container
- run a basic daemon + CLI flow as a non-root user

## Build

```bash
docker build -f packaging/container/Dockerfile.debian-test -t runloop-deb-test .
```

## Run

```bash
docker run --rm -t runloop-deb-test ./scripts/acceptance_debian_container.sh
```

Notes:
- The container uses Debian trixie to match the packaging target.
- The acceptance script runs `runloopd` directly (no systemd) and verifies that
  a user in the `runloop` group can connect to the daemon socket.
- The smoke test opening is `/etc/runloop/openings/smoke_exec.yaml` and runs
  the `system_helper` agent with a no-op host command.
