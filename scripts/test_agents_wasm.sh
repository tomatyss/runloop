#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

"$ROOT/scripts/build_agents_wasm.sh"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

TRACE_PATH="$TMP_DIR/trace.json"
TMP_CONFIG="$TMP_DIR/config.yaml"
mkdir -p "$TMP_DIR/runtime" "$TMP_DIR/sock" "$TMP_DIR/kb"
cat <<YAML >"$TMP_CONFIG"
version: 1
runtime:
  workdir: "$TMP_DIR/runtime"
  sockets_dir: "$TMP_DIR/sock"
kb:
  root_dir: "$TMP_DIR/kb"
security:
  confirm_external_actions: false
YAML
export RUNLOOP_CONFIG="$TMP_CONFIG"

echo "running compose_email via rlp --local..."
cargo run -q -p rlp -- run examples/openings/compose_email.yaml --local --params '{"recipient":"john"}' --trace-out "$TRACE_PATH" >"$TMP_DIR/run.ndjson"

python3 - <<'PY' "$TRACE_PATH"
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as fh:
    trace = json.load(fh)

if not trace.get("success"):
    raise SystemExit("compose_email run did not succeed")

node_states = {node["node_id"]: node["state"] for node in trace.get("nodes", []) if "node_id" in node}
for required in ("contacts", "context", "draft", "review", "send"):
    state = node_states.get(required)
    if state not in ("Succeeded", "Running", "Pending"):
        raise SystemExit(f"node {required} missing from trace")
print("wasm agents compose_email trace ok")
PY
