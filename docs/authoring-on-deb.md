# Authoring an agent on a Debian install

This is the minimal flow for creating and running a custom agent when Runloop is
installed from the `.deb` packages (no source checkout required).

## Prerequisites

- Rust toolchain installed via `rustup`.
- WebAssembly target installed once: `rustup target add wasm32-wasip1`.
- Runloop packages (`runloopd`, `rlp`, `agtop`) installed from the `.deb`.
- The packages ship the default `compose_email` bundles under
  `/usr/lib/runloop/agents`, plus writable copies under
  `/var/lib/runloop/agents`. The openings land at
  `/etc/runloop/openings/compose_email.yaml` and
  `/etc/runloop/openings/smoke_exec.yaml`, with writable copies under
  `/var/lib/runloop/openings`.
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
   - Agent bundle under the first writable `agents.search_dirs` entry (typically
     `/var/lib/runloop/agents/my_agent/` for users in the `runloop` group, else
     `~/.runloop/agents/my_agent/`) with `manifest.toml`, `policy.caps`,
     `tools.json`, `bin/`.
   - `~/.runloop/agents-wasm/my_agent/` crate with a stub `src/main.rs`.
   - Optional starter opening YAML under the first writable openings search dir
     (for example, `/var/lib/runloop/openings/<name>.yaml` or
     `~/.runloop/examples/openings/<name>.yaml`) if requested.

2. **Author**
   - Edit `~/.runloop/agents-wasm/my_agent/src/main.rs` with your logic.
   - Adjust capabilities in your bundle's `policy.caps`.
   - Add tools in your bundle's `tools.json` (version 1).

3. **Build + install into the bundle**

   ```bash
   rlp agent build my_agent
   ```

   The command compiles the wasm
   (`cargo build --release --target wasm32-wasip1`), copies it into the bundle
   `bin/` directory, recomputes BLAKE3 digests for `entry_wasm` and
   `tools.json`, and validates the caps file.

4. **Run**
   - If you generated a starter opening (use the path printed by the scaffold
     command, or check your writable openings dir):
     `rlp run /path/to/openings/my_agent.yaml --params '{"prompt":"..."}'`
   - Otherwise wire the agent into an opening and run it via
     `rlp run <opening.yaml>`.

## Picking an executor (local vs daemon)

- `rlp ... --local` uses the same registry search dirs as the daemon. When
  scaffolding, Runloop picks the first writable `agents.search_dirs` entry. On
  Debian installs this is typically `/var/lib/runloop/agents` for users in the
  `runloop` group; otherwise it falls back to `~/.runloop/agents` unless you
  pass `--agents-dir`.
- The packaged daemon runs as user `runloop` with home `/var/lib/runloop`, and
  its default agent search dirs resolve under that home. Agents you scaffolded
  in `/home/<you>/.runloop/agents` will not be visible to the daemon until you
  point the daemon at them or install into a daemon-owned search dir.

## Running your agent with the packaged systemd service

1. Install your bundle into the daemon-visible registry and drop your opening
   into a system search dir:

```bash
sudo rlp agent install --root /var/lib/runloop/agents <bundle.tar>
sudo install -m 0644 /path/to/my_agent.yaml /var/lib/runloop/openings/my_agent.yaml
```

`/var/lib/runloop/{agents,openings}` is group-writable for `runloop`, so users
in that group can manage bundles and openings without changing system config.

1. Make sure your user can connect to `/run/runloop/rmp.sock`:
   - Add yourself to the `runloop` group: `sudo usermod -a -G runloop "$USER"`
     (open a new shell so the group is applied).
   - The socket is created with `0660` permissions for `runloop:runloop`, so
     group membership is required.

1. Run your opening (no `--local` so it goes through the daemon):

```bash
rlp run /var/lib/runloop/openings/my_agent.yaml --params '{"prompt":"..."}'
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
