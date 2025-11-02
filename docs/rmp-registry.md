# RMP Schema Registry (Draft)

> **Doc status:** Draft — updates require review. Last updated: 2025-11-02.

Schema identifiers (`schema_id`) are 16-bit unsigned integers carried in every RMP header. The ranges below reserve space for core, extensions, and vendor-defined payloads.

| Range | Ownership | Notes |
| ----- | --------- | ----- |
| `0x0000` | Reserved | Never used; helps catch default/zero bugs. |
| `0x0001–0x000F` | **Core, stable** | Runloop-owned; changes require an ADR and backward-compatible plan. |
| `0x0010–0x007F` | First-party extensions | Experimental Runloop schemas that may graduate to Core. |
| `0x0080–0x03FF` | Local/private | Safe for development; not guaranteed to interoperate across installs. |
| `0x0400–0x0FFF` | Third-party provisional | External vendors reserve here via PR to this registry. |
| `0x1000–0xFFFF` | Vendor-defined | Must embed `schema_hash` and `vendor` fields in the payload. |

## Core assignments (effective)

| `schema_id` | Name | Version field | Summary |
| ----------- | ---- | ------------- | ------- |
| `0x0001` | `Observation` | `v` (u16) | Facts gathered by an agent about the environment. |
| `0x0002` | `Intent` | `v` (u16) | Requested action from router/opening to an agent. |
| `0x0003` | `ToolCall` | `v` (u16) | Structured tool invocation request. |
| `0x0004` | `ToolResult` | `v` (u16) | Result of a tool invocation, streaming or final. |
| `0x0005` | `Artifact` | `v` (u16) | Durable content blob metadata + pointer. |
| `0x0006` | `Critique` | `v` (u16) | Quality feedback / review of an artifact or intent. |
| `0x0007` | `StateDelta` | `v` (u16) | Mutation against agent/opening state machine. |
| `0x0008` | `Control.Heartbeat` | `v` (u16) | Health ping between runtime and agent. |
| `0x0009` | `Control.Ack` | `v` (u16) | Reliability acknowledgement frame. |
| `0x000A` | `Control.Error` | `v` (u16) | Negative acknowledgement with `code` + human-readable `message`. |
| `0x000B` | `Plan.OpeningSpec` | `v` (u16) | Serialized opening DAG for replay or inspection. |
| `0x000C` | `Plan.NodeStatus` | `v` (u16) | Streaming state transitions for plan nodes. |
| `0x000D` | `Metrics.Span` | `v` (u16) | Telemetry span emitted by agents/runtime. |
| `0x000E–0x000F` | Reserved | — | Future core payloads. |

Each payload MUST include a version field named `v` (u16). Additive changes stay within the same `schema_id`; breaking changes require a new `schema_id` or a version bump accompanied by backward-compat logic.

## Registration process

1. Propose new schema via PR updating this file (and ideally `docs/message-protocol.md`).
2. For third-party provisional IDs (`0x0400–0x0FFF`), include:
   - Contact info / org name
   - Schema summary and stability expectations
   - Sample payload
3. For vendor-defined IDs (`0x1000+`), Runloop runtime enforces the presence of `schema_hash` (e.g., SHA-256 of canonical schema) and `vendor` string inside the payload body.

## Test vectors

Add fixtures under `tests/fixtures/rmp/` (not yet in repo) using the schema IDs above. Each vector should contain:

- RMP frame (hex/base64)
- Parsed JSON for header/body
- Expected signature status (if any)

These fixtures drive interoperability tests for SDKs.
