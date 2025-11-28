#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${RUNLOOP_WORKSPACE_ROOT:-}" ]]; then
  ROOT="$(cd "${RUNLOOP_WORKSPACE_ROOT}" && pwd)"
else
  ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi
WASM_TARGET="wasm32-wasip1"
TARGET_DIR="$ROOT/target/$WASM_TARGET/release"
export CARGO_TARGET_DIR="$ROOT/target"
ALLOW_MISSING_MANIFESTS="${ALLOW_MISSING_MANIFESTS:-0}"

if ! rustc --print target-list | grep -q "$WASM_TARGET"; then
  echo "rustc missing $WASM_TARGET target (install via 'rustup target add $WASM_TARGET')" >&2
  exit 1
fi

discover_agents() {
  local -a paths=()
  local dir_name agent_name manifest
  while IFS= read -r entry; do
    [[ -f "$entry/Cargo.toml" ]] || continue
    if ! grep -q "\[\[bin\]\]" "$entry/Cargo.toml"; then
      continue
    fi
    dir_name="$(basename "$entry")"
    agent_name="${dir_name//-/_}"
    manifest="$ROOT/agents/$agent_name/manifest.toml"
    if [[ ! -f "$manifest" ]]; then
      if [[ "$ALLOW_MISSING_MANIFESTS" == "1" ]]; then
        echo "skipping $dir_name; missing manifest at agents/$agent_name/manifest.toml (ALLOW_MISSING_MANIFESTS=1)" >&2
        continue
      fi
      echo "missing manifest for $dir_name at agents/$agent_name/manifest.toml" >&2
      exit 1
    fi
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
  agent_dir_name="$(basename "$agent_dir")"
  agent_name="${agent_dir_name//-/_}"
  bin_name="${agent_name}_wasm"
  cargo build --release \
    --target "$WASM_TARGET" \
    --manifest-path "$agent_dir/Cargo.toml" \
    --bin "$bin_name"
done

echo "copying artifacts..."
for agent_dir in "${AGENT_DIRS[@]}"; do
  agent_dir_name="$(basename "$agent_dir")"
  agent_name="${agent_dir_name//-/_}"
  bin_name="${agent_name}_wasm"
  src="$TARGET_DIR/${bin_name}.wasm"
  dest_dir="$ROOT/agents/$agent_name/bin"
  dest="$dest_dir/${agent_name}.wasm"
  manifest="$ROOT/agents/$agent_name/manifest.toml"
  tools="$ROOT/agents/$agent_name/tools.json"

  if [[ ! -f "$manifest" ]]; then
    echo "manifest missing for agent $agent_name at $manifest" >&2
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
  perl -0pi -e "s#entry_wasm = \\{.*?\\}#entry_wasm = { path = \"bin/${agent_name}.wasm\", blake3 = \"$digest\" }#s" \
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
