# Tools

Reference material for the developer tooling that ships with Runloop.

- [Tool Attachments](tool-attachments.md) describes the `tools.json` schema used
  by `rlp agent scaffold` and bundle packaging.
- [TUI](tui.md) documents the interactive terminal UI for inspecting agents and
  workloads.

## Agent scaffold

`rlp agent scaffold <name>` runs an interactive wizard (unless
`--non-interactive`) to bootstrap a wasm agent skeleton:

- Prompts for model/provider route + secret hints, capability grants
  (fs/net/kb), and optional tool attachments (`tools.json` entries).
- Emits bundle files in `agents/<name>/`: `manifest.toml` (placeholder
  `entry_wasm` digest), `policy.caps` (based on responses), `tools.json` (v1),
  README, and `bin/.gitkeep`.
- Drops a companion crate in `crates/agents-wasm/<name>/` with a stub `main.rs`
  that signals readiness and prints placeholder JSON.
- Generates a starter opening YAML (under `examples/openings/`) wired to the new
  agent with a single node and `exists(node.out)` success condition.
- Digests are computed for `tools.json`; `entry_wasm` stays zeroed until
  `just build-agents-wasm` rebuilds the `.wasm` and updates the manifest.

Flags:

- `--root <path>` overrides the bundle root (defaults to the first
  `agents.search_dirs` entry from config).
- `--crates-dir <path>` overrides the wasm crate root (defaults to
  `crates/agents-wasm`).
- `--opening-path <path>` writes the starter opening to a custom location
  (defaults to `examples/openings/<name>.yaml` when generated).
- `--model`, `--cap-fs`, `--cap-net`, `--cap-kb-read`, `--cap-kb-write` seed
  wizard values or bypass prompts when `--non-interactive`.
- `--model-secret` overrides the provider secret id in non-interactive mode.
- `--force` allows scaffolding into existing paths (overwrites files).

Global CLI overrides (local runs only):

- `--agents-dir` prepends a search directory for agent bundles (ahead of
  `agents.search_dirs` from config). For daemon runs, configure the daemon with
  the same search dir instead.
- `--openings-dir` prepends a search directory for openings (ahead of
  `openings.search_dirs`); requires `--local`.
