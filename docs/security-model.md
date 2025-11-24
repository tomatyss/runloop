# Security Model (Draft)

> **Doc status:** Draft — normative for v0.1 where explicitly labeled. Last
> updated: 2025‑11‑02.

## Sandbox & Capabilities _(normative)_

- Agents execute inside **WASM/WASI** sandboxes hosted by Wasmtime with
  capability-based host calls.
- Capability manifests (`policy.caps`) are deny-by-default. Operators may only
  **remove** grants via overrides.
- Capability families (normative set):
  - `fs` (scoped path lists)
  - `net` (hostname allowlists; off by default)
  - `time`
  - `kb_read` / `kb_write` (domain lists, e.g., `contacts`, `artifacts`)
  - `secrets` (SecretStore IDs)
  - `model` (broker usage)
  - `exec` (disabled in v0.1)
- The runtime enforces capability checks at hostcall boundaries and records
  denials as `cap.audit` ledger events (and structured logs) when
  `security.caps.audit_on_deny` is `true` (default). Operators may also enable
  `security.caps.audit_on_allow` to persist allow decisions for high-scrutiny
  agents.
- _Implementation status:_ the `runloop-runtime` crate embeds Wasmtime, enforces
  capability checks for every exposed hostcall, and records denials as
  `cap.audit` events via the knowledge base (see
  `crates/runtime/tests/capabilities.rs`).

## Secret Handling _(normative)_

- Secret material never lives in the POG; only opaque `secret_id` references are
  stored.
- Default provider today is `security.secrets.provider = "stub"` (in-memory,
  test-only). Other providers are being implemented; an **env** provider exists
  for development and returns the raw environment value to agents—treat it as a
  convenience only, not a hardened backend.
- Planned `auto` backend will probe Secret Service → `pass` → age vault
  (`~/.runloop/secrets/`); not yet shipped.
- CLI (`rlp secrets put/get/list/delete`) is planned; until then, provision
  secrets through the chosen backend directly.
- Overrides may only **reference** existing secret IDs; agents cannot read
  arbitrary secrets without explicit capability grants.

## Provenance & Audit _(normative)_

- Every agent message carries RMP provenance metadata (`model`, `provider`,
  `parameters`, `tooling`).
- All writes into the POG ledger (`events.sqlite`) include BLAKE3 content hashes
  and source identifiers.
- Structured JSON logs include `trace_id`, `opening_id`, `agent_id`; redaction
  filters scrub secrets or PII patterns before sink.
- Optional Ed25519 signatures protect message integrity when crossing trust
  boundaries or when `security.require_signed_messages = true`.

## Threat Model (v0)

### Assumptions

- Host OS (Debian 12) is trusted and kept patched.
- Agents are untrusted code but must pass the WASM validator.
- Operators can inspect and reset the environment; hardware physical security is
  out of scope.

### In-scope threats

- **Malicious or compromised agent** attempting to exfiltrate data outside
  granted capabilities.
- **Supply-chain tampering** of agent bundles (mitigated via manifest
  signatures + SBOM).
- **Secrets disclosure** through logs or unredacted artifacts.
- **Privilege escalation** via hostcall misuse.

### Mitigations

- WASM sandbox with constrained syscalls; hostcalls check capability bitsets
  each invocation.
- Agent bundles are signed; `runloopd` verifies signatures before
  install/launch.
- Runloopd enforces outbound confirmation (`confirm_external_actions`) and
  tripwires (network/FS volume thresholds).
- Structured logging + telemetry scrubbing; security tests include fuzzing
  denied syscalls.

### Out-of-scope (v0)

- Kernel-level exploits or side-channel attacks in Wasmtime/CPU.
- Multi-tenant isolation across different Unix users or hosts.
- Compromise of external LLM providers or user-supplied secrets outside
  Runloop’s control.

## Package trust & signatures _(normative)_

- Agent bundles must ship an Ed25519 signature over `manifest.toml` (canonical
  form) and referenced digests.
- Trust anchors live in `~/.runloop/trust-policy.toml`; install/launch flows
  refuse bundles without a matching, non-revoked key.
- Capabilities permitted are constrained by trust rules (see `docs/ops.md`
  overall policy).
- First-party release keys rotate via signed keyset files; runtime caches latest
  keyset hash to detect downgrade attacks.

## Telemetry & Privacy (informative)

- Telemetry is opt-in; default is local-only.
- When enabled, OTLP exporters remove `secret_id`, raw prompt text is hashed,
  and cost metrics are aggregated per opening.
