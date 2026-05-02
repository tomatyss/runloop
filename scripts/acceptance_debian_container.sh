#!/usr/bin/env bash
set -euo pipefail

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing command: $1" >&2
    exit 1
  fi
}

require_cmd cargo
require_cmd dpkg

if [ "$(id -u)" -ne 0 ]; then
  echo "run as root inside the container" >&2
  exit 1
fi

scripts/build_agents_wasm.sh

cargo deb -p runloopd
cargo deb -p rlp
cargo deb -p agtop
cargo deb -p runloop

# Install packages (dpkg does not resolve deps; they are preinstalled in the image).
dpkg -i target/debian/runloopd_*.deb target/debian/rlp_*.deb target/debian/agtop_*.deb target/debian/runloop_*.deb

# Ensure runtime dirs exist even without systemd-tmpfiles.
install -d -o runloop -g runloop -m 0775 /run/runloop
install -d -o runloop -g runloop -m 0775 /var/lib/runloop/agents
install -d -o runloop -g runloop -m 0775 /var/lib/runloop/openings
install -d -o runloop -g runloop -m 0775 /var/lib/runloop/pog
install -d -o runloop -g runloop -m 0775 /var/lib/runloop/pog/vectors

if ! id -u testuser >/dev/null 2>&1; then
  useradd -m -G runloop testuser
fi

run_as_user() {
  local user="$1"
  shift
  if command -v runuser >/dev/null 2>&1; then
    runuser -u "$user" -- "$@"
    return
  fi
  if command -v su >/dev/null 2>&1; then
    su -s /bin/sh "$user" -c "$*"
    return
  fi
  echo "missing runuser/su to switch users" >&2
  exit 1
}

RUNLOOP_LOG=/tmp/runloopd.log
TRACE_OUT=/tmp/runloop_trace.json
run_as_user runloop sh -c "umask 0002; RUNLOOP_CONFIG=/etc/runloop/config.yaml RUST_LOG=debug exec runloopd" \
  >"$RUNLOOP_LOG" 2>&1 &
RUNLOOP_PID=$!

sleep 2

if ! run_as_user testuser sh -c "umask 0002; RUNLOOP_CONFIG=/etc/runloop/config.yaml exec rlp run /etc/runloop/openings/smoke_exec.yaml --trace-out '$TRACE_OUT'"; then
  echo "opening run failed; runloopd log:" >&2
  tail -n 200 "$RUNLOOP_LOG" >&2 || true
  if [ -f "$TRACE_OUT" ]; then
    echo "opening trace:" >&2
    cat "$TRACE_OUT" >&2 || true
  else
    echo "opening trace not materialized" >&2
  fi
  kill "$RUNLOOP_PID" || true
  wait "$RUNLOOP_PID" || true
  exit 1
fi

kill "$RUNLOOP_PID"
wait "$RUNLOOP_PID" || true

echo "container acceptance flow complete"
