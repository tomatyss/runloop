# Project Overview

Runloop OS is an agent-native operating layer that sits above your existing
Linux system. It routes prompts either to the POSIX shell or to **openings**:
declarative DAGs of WASM agents running with least-privilege capabilities over a
typed local message bus.

## What ships in this repo

- **Daemon and CLI:** `crates/runloopd` hosts the runtime; `crates/rlp` drives
  routing, KB commands, tracing, and local execution (`--local`).
- **Agent bundles:** canonical bundles live under `agents/`; companion wasm
  crates are under `crates/agents-wasm/`. Manifests, capability policies, and
  optional `tools.json` files sit next to each bundle.
- **Openings and scenarios:** runnable DAGs live in `examples/openings/` and
  double as fixtures for the planner and executor.
- **Shared libraries:** the message bus, protocol codecs, KB APIs, and model
  broker are in `crates/` (`rmp`, `bus`, `kb`, `model-broker`, etc.).
- **Docs:** mdBook sources live in `docs/`; the roadmap and ADRs sit alongside
  specifications for openings, KB schemas, and security policy.

For deeper design context, see `DESCRIPTION.md` in the repository root or the
architecture overview in `docs/architecture.md`.
