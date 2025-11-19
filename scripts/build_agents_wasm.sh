#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WASM_TARGET="wasm32-wasip1"
TARGET_DIR="$ROOT/target/$WASM_TARGET/release"
AGENTS=(contact_resolver context_gatherer writer critic mailer)

export CARGO_TARGET_DIR="$ROOT/target"

if ! rustc --print target-list | grep -q "$WASM_TARGET"; then
  echo "rustc missing $WASM_TARGET target (install via 'rustup target add $WASM_TARGET')" >&2
  exit 1
fi

echo "building wasm agents..."
for agent in "${AGENTS[@]}"; do
  cargo build --release \
    --target "$WASM_TARGET" \
    --manifest-path "$ROOT/crates/agents-wasm/${agent}/Cargo.toml" \
    --bin "${agent}_wasm"
done

echo "copying artifacts..."
for agent in "${AGENTS[@]}"; do
  bin_name="${agent}_wasm"
  src="$TARGET_DIR/${bin_name}.wasm"
  dest_dir="$ROOT/agents/$agent/bin"
  dest="$dest_dir/${agent}.wasm"
  mkdir -p "$dest_dir"
  cp "$src" "$dest"
  rm -f "$dest_dir/.gitkeep"
  digest="$(cargo run -q -p b3sum -- "$dest" | awk '/^[0-9a-f]{64}([[:space:]]|$)/ { print $1; exit }')"
  if [[ -z "$digest" ]]; then
    echo "failed to compute blake3 digest for $dest" >&2
    exit 1
  elif [[ ! "$digest" =~ ^[0-9a-f]{64}$ ]]; then
    echo "received malformed blake3 digest '$digest' for $dest" >&2
    exit 1
  fi
  perl -0pi -e "s#entry_wasm = \\{.*?\\}#entry_wasm = { path = \"bin/${agent}.wasm\", blake3 = \"$digest\" }#s" \
    "$ROOT/agents/$agent/manifest.toml"
done

echo "wasm agents updated."
