# tmux_layout

Adds a managed tmux config block with layout-friendly defaults and optionally
reloads tmux plus a live layout change.

**Inputs (`with`):**

- `preset` — `sensible` (default), `minimal`, or `stacked`.
- `tmux_conf` — target config path (defaults to `~/.tmux.conf`).
- `reload` — reload the config after writing (default `true`).
- `apply_layout` — optional live layout to run (`tiled`, `even-horizontal`, etc.).
- `extra_lines` — array of additional tmux commands appended after the preset.

**Output (`artifact.tmux_layout.result.v1`):**

```json
{
  "tmux_conf": "/home/user/.tmux.conf",
  "preset": "sensible",
  "reload": true,
  "apply_layout": "tiled",
  "updated": true,
  "snippet_lines": 22,
  "exec": [
    { "command": "tmux source-file /home/user/.tmux.conf", "exit_code": 0, "stdout": "", "stderr": "" },
    { "command": "tmux select-layout tiled", "exit_code": 0, "stdout": "", "stderr": "" }
  ]
}
```

**Capabilities:** exec (tmux reload/layout), fs (`~/.tmux.conf`, `~/.config/tmux`).
No model or network access.

Build the wasm + refresh digests:

```
rlp agent build tmux_layout
```

Run the sample opening:

```
rlp run examples/openings/tmux_layout.yaml --local \
  --params '{"preset":"sensible","apply_layout":"tiled"}'
```
