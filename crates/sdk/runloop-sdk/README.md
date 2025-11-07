# Runloop SDK & Shim

The `runloop-sdk` crate exposes capability- and protocol-aware helpers that
native (non-WASM) agents can use while running behind the host shim. It ships
with a companion binary, `agent-shim`, which bootstraps native agents under the
same capability envelope the WASM runtime enforces.

## Crate layout

- `runloop-sdk`
  - Capability manifest parsing (`caps`)
  - Shim/runtime handshake payloads (`handshake`)
  - `ShimClient` for publishing/consuming RMP frames via the bus (`shim`)
- `agent-shim`
  - CLI bootstrap that loads the effective caps from the runtime env, connects
    to the bus, emits a handshake, and then launches the native agent process.

## Environment contract

`agent-shim` (and native agents launched through it) expect the runtime to
provide these environment variables:

| Variable             | Meaning                                       |
| -------------------- | --------------------------------------------- |
| `RUNLOOP_SOCKET`     | Path/key of the in-process bus binding        |
| `RUNLOOP_AGENT_ID`   | UUID of the logical agent instance            |
| `RUNLOOP_CAPS_JSON`  | JSON blob describing the effective caps set   |
| `RUNLOOP_SHIM_VERSION` | Optional override for the shim version tag |

The JSON schema for `RUNLOOP_CAPS_JSON` matches `runloop_sdk::caps::EffectiveCaps`.

## CLI usage

```shell
$ RUNLOOP_SOCKET=/tmp/runloop-bus \
  RUNLOOP_AGENT_ID=5ab4e2dd-3e3a-4f6b-abd3-0d0f0b1c2aa4 \
  RUNLOOP_CAPS_JSON='{"fs":[{"root":"/home/user","write":true}]}' \
  agent-shim ./bin/contact-resolver --flag=demo
```

The shim will:

1. Parse the caps manifest and connect to the bus as an `agent` publisher.
2. Emit an `agent.hello` payload on `rlp/runtime/hello` describing the active
   caps and shim version.
3. Launch the requested command, inheriting stdio and environment.
4. Exit with the agent process' status code.

## Testing

`cargo test -p runloop-sdk` exercises the capability parser, handshake encode /
decode, and a publish/subscribe loop against an in-process bus binding.
