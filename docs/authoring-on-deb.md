# Authoring an agent on a Debian install

This is the minimal flow for creating and running a custom agent when Runloop is
installed from the `.deb` packages (no source checkout required).

## Prerequisites

- Rust toolchain installed via `rustup`.
- WebAssembly target installed once: `rustup target add wasm32-wasip1`.
- Runloop packages (`runloopd`, `rlp`, `agtop`) installed from the `.deb`.

## Steps

1. **Scaffold**

   ```bash
   rlp agent scaffold my_agent
   ```

   This creates:
   - `~/.runloop/agents/my_agent/` bundle with `manifest.toml`, `policy.caps`,
     `tools.json`, `bin/`.
   - `~/.runloop/agents-wasm/my_agent/` crate with a stub `src/main.rs`.
   - Optional starter opening YAML if requested.

2. **Author**
   - Edit `~/.runloop/agents-wasm/my_agent/src/main.rs` with your logic.
   - Adjust capabilities in `~/.runloop/agents/my_agent/policy.caps`.
   - Add tools in `~/.runloop/agents/my_agent/tools.json` (version 1).

3. **Build + install into the bundle**

   ```bash
   rlp agent build my_agent
   ```

   The command compiles the wasm
   (`cargo build --release --target wasm32-wasip1`), copies it into
   `~/.runloop/agents/my_agent/bin/`, recomputes BLAKE3 digests for `entry_wasm`
   and `tools.json`, and validates the caps file.

4. **Run**
   - If you generated a starter opening:
     `rlp run examples/openings/my_agent.yaml --params '{"prompt":"..."}'`
   - Otherwise wire the agent into an opening and run it via
     `rlp run <opening.yaml>`.

## Notes

- `rlp config path --all` shows which config layers are active; unreadable
  `/etc/runloop/config.yaml` is skipped with a warning.
- To rebuild after edits, rerun `rlp agent build <name>`; it will refresh the
  wasm and manifest digests.
- Bundle/crate roots can be overridden with `--root` / `--crates-dir` flags if
  you keep agents outside `~/.runloop`.
