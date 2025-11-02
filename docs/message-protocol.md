# Runloop Message Protocol (Draft)

> **Doc status:** Draft — normative for framing, headers, and schema handling in v0.1. Last updated: 2025-11-02.

The Runloop Message Protocol (RMP) is a binary envelope for agent messages. It provides:

1. A fixed wire frame for transport-level metadata.
2. A MsgPack-encoded header map with routing, budget, and provenance fields.
3. A schema-referenced body (MsgPack by default) with optional signatures and compression.

## 1. Wire framing (normative)

All multi-byte integers are big-endian.

```
+-----------------+-----------+-----------+-------------+-------------+-----------+--------------------+
| Magic "RMP1"    | ver u16   | flags u16 | hdr_len u32 | body_len u32| sig_len u16 | PAYLOAD ...       |
+-----------------+-----------+-----------+-------------+-------------+-----------+--------------------+
| Header bytes (MsgPack map) | Body bytes | Signature (optional)                          |
```

- **Magic:** ASCII `RMP1` (0x52 0x4D 0x50 0x31)
- **ver:** frame version (initially `1`)
- **flags:** bitmask (bit 0 = signed, bit 1 = compressed-zstd, others reserved)
- **hdr_len / body_len:** lengths in bytes of the MsgPack header/body payloads
- **sig_len:** `0` (no signature) or `64` for Ed25519 signatures. Additional signature algorithms require a new flag + length convention.
- **Signature input:** the concatenation of everything before the signature field (magic through body bytes).

## 2. Header map (MsgPack, normative)

Every header MUST contain the following keys.

| Key | Type | Description |
| --- | ---- | ----------- |
| `v` | u16 | Header schema version (currently `1`). |
| `schema_id` | u16 | Payload schema (see [Schema registry](rmp-registry.md)). |
| `msg_id` | bytes(16) | UUIDv7 (or equivalent monotonic) identifier scoped to sender. |
| `trace_id` | bytes(16) | UUIDv7 trace identifier for end-to-end observability. |
| `opening_id` | u64 | Opening/run identifier (0 if not in an opening). |
| `from` | str | Sender identity (`agent:<name>@<version>` or `router`). |
| `to` | str | Recipient (`agent:<name>` or bus topic). |
| `ttl_ms` | u32 | Time-to-live relative to `ts`. |
| `ts` | u64 | Unix time in nanoseconds when header created. |
| `body_hash` | bytes(32) | BLAKE3-256 of body bytes after compression. |

Recommended keys (processed when present):

| Key | Type | Notes |
| --- | ---- | ----- |
| `budget` | map | `{ "tokens": u32, "usd": f32, "wall_ms": u32 }`. |
| `caps` | map | Capability snapshot, e.g. `{ "kb_read": ["contacts"], "model": true }`. |
| `provenance` | map | `{ "model": str, "provider": str, "parameters": map, "tooling": [str] }`. |
| `deadline_unix_ms` | u64 | Absolute deadline. |
| `priority` | u8 | 0 (default) – 7 (highest). |
| `qos` | str | `"durable"` or `"ephemeral"`. |
| `reply_to` | str | Optional reply subject. |

Optional/experimental keys MUST be documented in `docs/rmp-registry.md` before production use.

## 3. Payload schemas & versions (normative)

- `schema_id` selects the payload shape. Core assignments live in [`docs/rmp-registry.md`](rmp-registry.md).
- Every payload MUST include a `v` (u16) version field inside the body. Additive changes stay within the same `schema_id`; breaking changes require a new `schema_id` (or explicit compatibility shim).
- Vendor-defined IDs (`0x1000+`) MUST embed `schema_hash` (e.g., SHA-256 of canonical schema) and `vendor` string in the body.

### 3.1 Core schemas preview (informative)

| Schema | `schema_id` | Body highlights |
| ------ | ----------- | --------------- |
| `Observation` | `0x0001` | `{ v, actor, readings[], confidence }` |
| `Intent` | `0x0002` | `{ v, goal, params, budget_hint }` |
| `ToolCall` | `0x0003` | `{ v, tool, args, invocation_id }` |
| `ToolResult` | `0x0004` | `{ v, invocation_id, result, error? }` |
| `Artifact` | `0x0005` | `{ v, artifact_id, mime, size, digest, blob_ref }` |
| `Critique` | `0x0006` | `{ v, subject_id, rating, notes }` |
| `StateDelta` | `0x0007` | `{ v, target, delta, reason }` |
| `Control.Heartbeat` | `0x0008` | `{ v, status, metrics }` |
| `Control.Ack` | `0x0009` | `{ v, acked_msg_id }` |
| `Control.Error` | `0x000A` | `{ v, failed_msg_id, code, message, retryable }` |
| `Plan.OpeningSpec` | `0x000B` | `{ v, opening_id, dag_yaml, checksum }` |
| `Plan.NodeStatus` | `0x000C` | `{ v, node_id, state, ts, message? }` |
| `Metrics.Span` | `0x000D` | `{ v, span_id, parent_id?, name, start_ns, end_ns, attributes{} }` |

Reference JSON schema drafts will land alongside implementation.

## 4. Signatures & compression (normative)

- **Signatures:** Ed25519 by default. Public keys are associated with agents via trust policy (`docs/ops.md`). When the signature flag is set, receivers MUST verify before processing.
- **Compression:** `flags` bit 1 indicates zstd compression of the body. Receivers MUST decompress before hashing/validation.

## 5. Delivery guarantees (informative)

- Intra-host bus uses Unix domain sockets; delivery is at-least-once. `Control.Ack` + `msg_id` provide idempotence.
- Router retries failed deliveries with exponential backoff unless `qos = "ephemeral"`.
- TTL expiration results in a `Control.Error` with code `TTL_EXPIRED` routed back to sender.

## 6. Schema registry & test vectors

See [`docs/rmp-registry.md`](rmp-registry.md) for reserved ranges and assignments. Tests MUST include fixtures for every core schema and a corruption case (bad hash, expired TTL, signature failure).

## 7. Backwards compatibility rules (normative)

1. **Header fields:** new optional keys are allowed; never remove or rename existing keys without bumping frame version.
2. **Bodies:** additive fields must be ignored by older agents; removing/renaming requires new `schema_id`.
3. **Flags:** toggling new bits requires documentation and negotiation via config; unknown bits MUST be ignored but logged with warning.

---

For questions or proposals, file an ADR referencing this document and update the registry table accordingly.
