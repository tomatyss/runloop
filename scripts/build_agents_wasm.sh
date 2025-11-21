#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WASM_TARGET="wasm32-wasip1"
TARGET_DIR="$ROOT/target/$WASM_TARGET/release"
export CARGO_TARGET_DIR="$ROOT/target"

if ! rustc --print target-list | grep -q "$WASM_TARGET"; then
  echo "rustc missing $WASM_TARGET target (install via 'rustup target add $WASM_TARGET')" >&2
  exit 1
fi

discover_agents() {
  local -a paths=()
  while IFS= read -r entry; do
    [[ -f "$entry/Cargo.toml" ]] || continue
    paths+=("$entry")
  done < <(find "$ROOT/crates/agents-wasm" -mindepth 1 -maxdepth 1 -type d ! -name target -print | sort)

  if [[ ${#paths[@]} -eq 0 ]]; then
    echo "no agent crates found under crates/agents-wasm" >&2
    exit 1
  fi

  AGENT_DIRS=("${paths[@]}")
}

compute_digest() {
  local path="$1"
  cargo run -q -p b3sum -- "$path" | awk '/^[0-9a-f]{64}([[:space:]]|$)/ { print $1; exit }'
}

update_tools_block() {
  local manifest="$1"
  local digest="$2"
  local rel_path="$3"
  perl -0pi -e '
    my $digest = $ENV{TOOLS_DIGEST};
    my $path = $ENV{TOOLS_PATH};
    my $replaced = 0;
    s{
      ^\[artifacts\.tools\]\n
      (?:[^\n]*\n)*?
      (?=^\[|\z)
    }{
      $replaced = 1;
      "[artifacts.tools]\npath = \"$path\"\nblake3 = \"$digest\"\nversion = 1\n"
    }gexm;
    if (!$replaced) {
      $_ .= "\n[artifacts.tools]\npath = \"$path\"\nblake3 = \"$digest\"\nversion = 1\n";
    }
  ' "$manifest"
}

discover_agents

echo "building wasm agents..."
for agent_dir in "${AGENT_DIRS[@]}"; do
  agent="$(basename "$agent_dir")"
  bin_name="${agent}_wasm"
  cargo build --release \
    --target "$WASM_TARGET" \
    --manifest-path "$agent_dir/Cargo.toml" \
    --bin "$bin_name"
done

echo "copying artifacts..."
for agent_dir in "${AGENT_DIRS[@]}"; do
  agent="$(basename "$agent_dir")"
  bin_name="${agent}_wasm"
  src="$TARGET_DIR/${bin_name}.wasm"
  dest_dir="$ROOT/agents/$agent/bin"
  dest="$dest_dir/${agent}.wasm"
  manifest="$ROOT/agents/$agent/manifest.toml"
  tools="$ROOT/agents/$agent/tools.json"

  if [[ ! -f "$manifest" ]]; then
    echo "manifest missing for agent $agent at $manifest" >&2
    exit 1
  fi

  mkdir -p "$dest_dir"
  cp "$src" "$dest"
  rm -f "$dest_dir/.gitkeep"

  digest="$(compute_digest "$dest")"
  if [[ -z "$digest" ]] || [[ ! "$digest" =~ ^[0-9a-f]{64}$ ]]; then
    echo "failed to compute blake3 digest for $dest" >&2
    exit 1
  fi
  perl -0pi -e "s#entry_wasm = \\{.*?\\}#entry_wasm = { path = \"bin/${agent}.wasm\", blake3 = \"$digest\" }#s" \
    "$manifest"

  if [[ -f "$tools" ]]; then
    tools_digest="$(compute_digest "$tools")"
    if [[ -z "$tools_digest" ]] || [[ ! "$tools_digest" =~ ^[0-9a-f]{64}$ ]]; then
      echo "failed to compute blake3 digest for $tools" >&2
      exit 1
    fi
    TOOLS_DIGEST="$tools_digest" TOOLS_PATH="$(basename "$tools")" update_tools_block "$manifest" "$tools_digest" "$(basename "$tools")"
  fi
done

echo "wasm agents updated."
