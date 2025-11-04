# Runloop Runtime

The `runloop-runtime` crate embeds Wasmtime and enforces the Runloop capability
model when spawning wasm32-wasi agents. It is responsible for:

- Initialising the Wasmtime engine/linker with WASI imports gated by the active
  `Caps`.
- Intersecting workspace policy and per-agent overrides before launch.
- Surfacing hostcall shims that respect capability decisions and emit audit
  events for denied operations.
- Bridging agent stdout/stderr into ring buffers for the CLI/TUI.
- Tracking per-agent resource usage (RSS/CPU) on a best-effort basis.
- Routing mailbox traffic to the bus when configured.

## Hostcalls

Custom hostcalls live under the `"runloop"` namespace. Each one checks the
agent's `Caps` before executing and records a `cap.audit` KB event when a
decision is denied.

| Hostcall           | Behaviour                                                         |
| ------------------ | ----------------------------------------------------------------- |
| `time_now`         | Returns wall-clock microseconds; denied if `Caps::time` is false. |
| `http_request`     | Allows HTTP(S) only to domains in `Caps::net_hosts`; HTTPS unless `allow_http`. |
| `kb_read` / `kb_write` | Verifies namespace access using `CapabilitySet`.             |
| `model_complete`   | Invokes the model broker. If the guest-supplied buffer is too small, the hostcall returns **`-(required_len)`** so the caller can grow its allocation. No bytes are written on that path. |
| `resolve_secret`   | Returns opaque secret identifiers when permitted.                 |
| `exec_spawn`       | Stubbed until exec caps are enabled; guard rail in place.         |
| `mailbox_recv`     | Pulls pending bus messages for the agent.                         |

Filesystem access is mediated by `WasiCtxBuilder::preopened_dir`. The runtime
only preopens directories that appear in the agent's filesystem capabilities
and chooses `DirPerms`/`FilePerms` (read-only vs read/write) based on each
entry's `write` flag. Guests therefore see just the permitted roots and cannot
traverse outside them.

Stdout/stderr are surfaced to callers via an async `StdoutStream` adapter that
mirrors guest writes into bounded `OutputRing` buffers while retaining a full
copy for later inspection.

## Audit Trail

Denied hostcalls emit structured `cap.audit` events via `runloop-kb` and log a
warning. Events contain the capability, operation, target, BLAKE3 hash of the
arguments, and the decision reason.

## Bus Integration

When constructed with a `Bus` handle, the runtime subscribes each agent to its
direct topic and publishes outbound messages through the bus. Calls to
`Runtime::send` block on the bus future so that delivery errors (e.g., closed
server, TTL rejection) propagate back to the caller.

## Statistics

`AgentStats` reports RSS and accumulated CPU time. Linux builds with the
`procfs` feature read per-thread stats from `/proc`. Other platforms fall back
to `sysinfo` and omit CPU totals when fine-grained data is unavailable.
