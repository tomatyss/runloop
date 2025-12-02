# Authoring an agent on a Debian install

This is the minimal flow for creating and running a custom agent when Runloop is
installed from the `.deb` packages (no source checkout required).

## Prerequisites

- Rust toolchain installed via `rustup`.
- WebAssembly target installed once: `rustup target add wasm32-wasip1`.
- Runloop packages (`runloopd`, `rlp`, `agtop`) installed from the `.deb`.
- Verify the CLI you installed exposes agent commands:

  ```bash
  rlp --version          # expects rlp >= 0.1.2
  rlp agent --help       # should list scaffold/build/list
  ```

  If `agent` is missing, reinstall from the latest `.deb`.

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

## Picking an executor (local vs daemon)

- `rlp ... --local` uses the built-in demo agents only; your new bundle will not
  be found there.
- The packaged daemon runs as user `runloop` with home `/var/lib/runloop`, and
  its default agent search dirs resolve under that home. Agents you scaffolded
  in `/home/<you>/.runloop/agents` will not be visible until you point the
  daemon at them.

## Running your agent with the packaged systemd service

1. Point the daemon at your bundle/openings and keep the socket reachable:

```bash
mkdir -p ~/.runloop
cat > ~/.runloop/config.yaml <<EOF
agents:
  search_dirs:
    - $HOME/.runloop/agents
openings:
  search_dirs:
    - $HOME/examples/openings   # adjust to where you keep openings
runtime:
  sockets_dir: /run/runloop   # matches the packaged service default
EOF
```

1. Add a systemd drop-in so the service uses your config and creates a
   group-writable socket:

```bash
sudo mkdir -p /etc/systemd/system/runloopd.service.d
cat <<EOF | sudo tee /etc/systemd/system/runloopd.service.d/override.conf
[Service]
Environment=RUNLOOP_CONFIG=$HOME/.runloop/config.yaml
UMask=0002
RuntimeDirectoryMode=0775
EOF
```

1. Make sure your user can connect to `/run/runloop/rmp.sock`:
   - Add yourself to the `runloop` group: `sudo usermod -a -G runloop "$USER"`
     (open a new shell so the group is applied).
   - Keep the socket group-writable via the drop-in above.

1. Ensure the daemon user can read your bundle:
   - Simplest: make your home traversable (`chmod 711 "$HOME"`) so
     `$HOME/.runloop/agents/...` is readable, or copy the bundle to a
     daemon-owned path such as `/usr/lib/runloop/agents/my_agent`.

1. Reload and restart the service:

```bash
sudo systemctl daemon-reload
sudo systemctl restart runloopd
```

1. Run your opening (no `--local` so it goes through the daemon):

```bash
rlp run "$HOME/examples/openings/my_agent.yaml" --params '{"prompt":"..."}'
```

If you prefer not to touch the system service, point both daemon and CLI at a
home-local socket instead:

- Update `~/.runloop/config.yaml` so the socket lives under your home (either
  `runtime.sockets_dir: $HOME/.runloop/sock` or
  `runtime.socket_path: $HOME/.runloop/sock/rmp.sock`). The CLI resolves the
  socket in this order: `runtime.socket_path`, then
  `${runtime.sockets_dir}/rmp.sock`, then `~/.runloop/sock/rmp.sock`, then
  `/run/runloop/rmp.sock`.
- Run your own daemon with that config:

```bash
runloopd --config ~/.runloop/config.yaml &
rlp run "$HOME/examples/openings/my_agent.yaml" --params '{"prompt":"..."}'
```

Use an env override only for a temporary socket change:
`RUNLOOP__RUNTIME__SOCKET_PATH=$HOME/.runloop/sock/rmp.sock rlp run ...`

## Notes

- `rlp config path --all` shows which config layers are active; unreadable
  `/etc/runloop/config.yaml` is skipped with a warning.
- To rebuild after edits, rerun `rlp agent build <name>`; it will refresh the
  wasm and manifest digests.
- Bundle/crate roots can be overridden with `--root` / `--crates-dir` flags if
  you keep agents outside `~/.runloop`.
