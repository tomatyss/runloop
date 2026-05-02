#!/usr/bin/env bash
set -euo pipefail

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing command: $1" >&2
    exit 1
  fi
}

require_cmd rlp
require_cmd runloopd
require_cmd systemctl

if ! id -nG "${USER}" | grep -q "\brunloop\b"; then
  echo "user '$USER' is not in the runloop group; run: sudo usermod -a -G runloop \"$USER\" and re-login" >&2
  exit 1
fi

sudo systemctl enable --now runloopd.service
sudo systemctl is-active --quiet runloopd.service

AGENTS_DIR="/var/lib/runloop/agents"
OPENINGS_DIR="/var/lib/runloop/openings"

echo "scaffolding agent..."
rlp agent scaffold note_taker \
  --non-interactive \
  --root "$AGENTS_DIR" \
  --opening-path "$OPENINGS_DIR/note_taker.yaml"

rlp agent build note_taker --root "$AGENTS_DIR"

rlp run /var/lib/runloop/openings/note_taker.yaml --params '{"prompt":"hello"}'

echo "acceptance flow complete"
