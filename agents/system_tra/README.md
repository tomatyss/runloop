# system_tra agent
Raise tmux and shell history limits with a managed config block.

Scaffolded by `rlp agent scaffold`. Build the wasm artifact with:

```
rlp agent build system_tra
```

Or manually:

```
cargo build --target wasm32-wasip1 --manifest-path /home/ivan/runloop/crates/agents-wasm/system_tra/Cargo.toml
```

Inputs (recommended: YAML object under `with.input`, serialized automatically):

```yaml
with:
  input:
    tmux_conf: "~/.tmux.conf"
    history_limit: 50000
    extra_tmux_lines:
      - "set -g mouse on"
    bashrc: "~/.bashrc"
    hist_size: 50000
    hist_file_size: 100000
```

Legacy: a raw JSON string in `with.input` is still accepted for compatibility.
Note: templated values (`{{params.*}}`) are only allowed when the entire string
is a single template token; embed templates by using the structured object
form above.

Outputs (stdout → `out` port):

```json
{
  "tmux": { "path": "/home/user/.tmux.conf", "updated": true },
  "history": { "path": "/home/user/.bashrc", "updated": true },
  "history_limit": 50000,
  "hist_size": 50000,
  "hist_file_size": 100000,
  "extra_tmux_lines": 1
}
```

Capabilities:

- fs: `~/.tmux.conf`, `~/.config/tmux`, `~/.bashrc`, `~/.runloop/artifacts/system_tra`
- exec/model/net/kb: disabled

Generated files:
- `/home/ivan/runloop/agents/system_tra/manifest.toml`
- `/home/ivan/runloop/agents/system_tra/policy.caps`
- `/home/ivan/runloop/agents/system_tra/tools.json`
- `/home/ivan/runloop/agents/system_tra/README.md`
- `/home/ivan/runloop/crates/agents-wasm/system_tra` (wasm implementation)

Re-run `rlp agent build system_tra` after edits to refresh the wasm and digest.
