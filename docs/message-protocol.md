# Runloop Message Protocol (Draft)

> **Doc status:** Draft — normative for framing, headers, and schema handling in v0.1. Last updated: 2025-11-02.

The Runloop Message Protocol (RMP) is a binary envelope for agent messages. It provides:

1. A fixed wire frame for transport-level metadata.
2. A MsgPack-encoded header map with routing, budget, and provenance fields.
3. A schema-referenced body (MsgPack by default) with optional signatures and compression.

## 1. Wire framing (normative)

RMP v0 uses a **fixed 60-byte header** followed by a MsgPack body. All integers are big-endian.

| Offset | Field | Type | Notes |
| ------ | ----- | ---- | ----- |
| 0 | Magic | `[u8;4]` | ASCII `RMP0` |
| 4 | `header_version` | `u16` | Always `0` for v0 |
| 6 | `header_len` | `u16` | Always `60` |
| 8 | `flags` | `u16` | bit 0 = signed, bit 1 = zstd; others reserved |
| 10 | `schema_id` | `u16` | Payload schema ID |
| 12 | `body_len` | `u32` | Length of the MsgPack body |
| 16 | `created_at_ms` | `u64` | UTC milliseconds when the header was produced |
| 24 | `ttl_ms` | `u32` | Relative TTL; `0` disables expiry |
| 28 | `trace_id` | `[u8;16]` | UUIDv7 recommended |
| 44 | `msg_id` | `[u8;16]` | UUIDv7 recommended |

Receivers MUST drop frames whose TTL has expired relative to the local clock and publish a drop event on `rlp/sys/drops`. Senders SHOULD generate monotonically increasing UUIDv7 IDs to keep dedupe caches efficient. Duplicate detection is performed on `(trace_id, msg_id)` pairs with an LRU cache per receiver.

The flags field reserves space for signatures and compression. Signatures and ACK/NAK handshakes ship as part of **RMP 0.2**; implementations MAY ignore unknown flag bits but MUST log them.

## 2. Body envelope (normative)

Body bytes are MsgPack maps of the form `{ "type": <schema_id>, "payload": <schema-specific> }`. The `type` field MUST equal the `schema_id` declared in the fixed header. Payload schemas are registered in [`docs/rmp-registry.md`](rmp-registry.md); additive changes within a schema are handled with in-payload versioning.

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
- Receivers emit drop metrics (`DropNotice{topic,reason,trace_id,msg_id}`) on `rlp/sys/drops` for TTL and duplicate rejections.

## 6. Schema registry & test vectors

See [`docs/rmp-registry.md`](rmp-registry.md) for reserved ranges and assignments. Tests MUST include fixtures for every core schema and a corruption case (bad hash, expired TTL, signature failure).

## 7. Backwards compatibility rules (normative)

1. **Header struct:** the 60-byte header is fixed; adding or reordering fields requires a new `header_version` (e.g., v1) and likely a different `header_len`.
2. **Bodies:** additive fields must be ignored by older agents; removing/renaming requires new `schema_id`.
3. **Flags:** toggling new bits requires documentation and negotiation via config; unknown bits MUST be ignored but logged with warning.

---

For questions or proposals, file an ADR referencing this document and update the registry table accordingly.
