# Agents

Agent bundles are the deployable units the runtime executes. Each bundle
declares its capabilities, optional host tool attachments, and a wasm binary
built from the companion crate.

## Bundle layout

- `agents/<name>/manifest.toml` — metadata plus `entry_wasm` digest.
- `agents/<name>/policy.caps` — capability allowlist (fs/net/kb/model, etc.).
- `agents/<name>/tools.json` — optional host tools exposed to the agent.
- `agents/<name>/bin/*.wasm` — compiled wasm binaries.
- `crates/agents-wasm/<name>/` — source crate that produces the wasm binary.
- Discovery defaults (user mode): `~/.runloop/agents` for bundles and
  `~/.runloop/examples/openings` for openings, with repo paths (`./agents`,
  `./examples/openings`) and system paths (`/var/lib/runloop/agents`) searched
  as fallbacks; use `--agents-dir`/`--openings-dir` to point `rlp` at custom
  locations.

Use `just build-agents-wasm` to rebuild the canonical bundles and refresh their
digests, or `just test-agents-wasm` for an end-to-end smoke test of the
`compose_email` opening.

## Scaffold → build → run (example)

The CLI can generate a new agent plus a starter opening wired to it. This flow
assumes a source checkout; see [authoring-on-deb.md](authoring-on-deb.md) for
the packaged install variant.

```bash
# 1) Scaffold bundle + crate + optional starter opening
rlp agent scaffold note_taker --opening

# 2) Implement your agent in crates/agents-wasm/note_taker/src/main.rs
#    (the stub prints placeholder JSON so the next step already succeeds)

# 3) Build and copy the wasm into the bundle, updating digests
rlp agent build note_taker

# 4) Run the generated opening locally to verify the wiring
rlp run examples/openings/note_taker.yaml --local \
  --params '{"prompt":"draft a standup note"}'
```

- `--root` and `--crates-dir` override the default bundle/crate roots.
- To add the agent to other openings, reference it via `use: agent:note_taker`
  and pass parameters in the node’s `with:` block.
- Capabilities are enforced at runtime; tighten `policy.caps` before shipping.

## Walkthrough: system settings agent (tmux/history)

Example flow for an agent that improves your tmux setup or extends shell history
by editing dotfiles and (optionally) running tmux to reload the config.

1. Scaffold (interactive wizard)

```bash
rlp agent scaffold system_tra --opening
```

Answer the prompts as you did (`model = google:gemini-2.5-flash`, blank network,
KB disabled). This creates:

- `agents/system_tra/` bundle (manifest, `policy.caps`, `tools.json`, bin/)
- `crates/agents-wasm/system_tra/` crate with a stub `main.rs`
- `examples/openings/system_tra.yaml` wired to the new agent

1. Grant the right capabilities

Edit `agents/system_tra/policy.caps` to allow the files you want to touch and,
if needed, host command execution. Use absolute paths (replace `/home/ivan` with
your home):

```toml
[capabilities]
model = true                  # keep model access if the agent uses LLMs
fs = [
  "/home/ivan/.tmux.conf",
  "/home/ivan/.config/tmux",
  "/home/ivan/.bashrc",        # for HISTSIZE/HISTFILESIZE tweaks
]
exec = true                    # required only if you will run tmux to reload
time = true                    # optional: timestamps for logs/decisions
net = []                       # stay offline for system tweaks
kb_read = false
kb_write = false
```

Drop `exec = true` if the agent will only edit files. Keep `fs` as narrow as
possible; add other paths (e.g., `/home/ivan/.zshrc`) as needed.

1. Implement the agent logic

The bundled implementation already:

- Parses `with.input` as JSON.
- Manages a tmux block bounded by `# >>> runloop:system_tra` / `# <<< …` and
  sets `history-limit`.
- Updates `HISTSIZE` / `HISTFILESIZE` in `~/.bashrc`.
- Emits a JSON result with file paths and whether each file was updated.

Tweak `crates/agents-wasm/system_tra/src/main.rs` if you want different defaults
(e.g., disable shell history edits) and widen `policy.caps` only when needed.

1. Build and run

```bash
rlp agent build system_tra
rlp run examples/openings/system_tra.yaml --local \
  --params '{"prompt":"raise tmux history limit to 50000"}'
```

The generated opening already passes a `prompt` param; adjust it or add more
structured fields under `with:` if your agent expects specific inputs.
