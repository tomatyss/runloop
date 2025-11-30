# system_tra agent
Update system settings

Scaffolded by `rlp agent scaffold`. Build the wasm artifact with:

```
rlp agent build system_tra
```

Or manually:

```
cargo build --target wasm32-wasip1 --manifest-path /home/ivan/runloop/crates/agents-wasm/system_tra/Cargo.toml
```

Model: `google:gemini-2.5-flash` (secret: <none>)
FS caps: ~/.runloop/artifacts/system_tra
Net caps: none
KB read: false
KB write: false

Generated files:
- `/home/ivan/runloop/agents/system_tra/manifest.toml`
- `/home/ivan/runloop/agents/system_tra/policy.caps`
- `/home/ivan/runloop/agents/system_tra/tools.json`
- `/home/ivan/runloop/agents/system_tra/README.md`
- `/home/ivan/runloop/crates/agents-wasm/system_tra` (wasm stub)
- `<skipped>`

Edit `/home/ivan/runloop/crates/agents-wasm/system_tra/src/main.rs` to implement your logic and
re-run `rlp agent build system_tra` to refresh digests.
