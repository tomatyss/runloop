# Runloop Message Protocol (RMP v0)

> **Doc status:** Normative for framing, headers, TTL/duplicate handling, and
> error taxonomy. Last updated: 2025-11-13.

RMP v0 defines a binary envelope for agent messages on the local Runloop bus.
The protocol is **frozen for v0** under the rules below.

## 1. Wire framing (normative)

- **Byte order:** every multi-byte integer is encoded in **big-endian (network
  order)**. Parsers MUST reject host-endian variants.
- **Transport:** streaming transports (Unix domain sockets by default). Frames
  may not be reordered or partially replayed.
- **Frame format:**
  1. `u32 frame_len` prefix that covers `header_len + body_len` (does **not**
     include the prefix itself).
  2. **Fixed 64-byte header** (see below).
  3. `body_len` bytes (MsgPack map body).

> If the prefix is ever removed in a future transport, `header_len + body_len`
> MUST remain the delimiter.

### 1.1 Fixed header (big-endian, 64 bytes)

<!-- markdownlint-disable MD013 -->

| Offset | Size | Field            | Notes                                                       |
| -----: | ---: | ---------------- | ----------------------------------------------------------- |
|      0 |    4 | `magic`          | ASCII **`"RMP0"`** (`0x52 0x4D 0x50 0x30`)                  |
|      4 |    2 | `header_version` | **0** for RMP v0; anything else → **UnsupportedVersion**    |
|      6 |    2 | `header_len`     | **64**, compare directly; mismatch → **UnsupportedVersion** |
|      8 |    4 | `flags`          | **MUST be 0 in v0**; non-zero → **InvalidHeaderFlags**      |
|     12 |    2 | `schema_id`      | u16 primitive family ID (see registry)                      |
|     14 |    2 | `reserved2`      | **0**; otherwise reject                                     |
|     16 |    4 | `body_len`       | u32 body length in bytes                                    |
|     20 |    8 | `created_at_ms`  | u64 epoch milliseconds                                      |
|     28 |    8 | `ttl_ms`         | u64 relative TTL; `0` → **InvalidTtl**                      |
|     36 |   16 | `trace_id`       | u128 routed end-to-end                                      |
|     52 |    8 | `msg_id`         | u64 monotonic per publisher                                 |
|     60 |    4 | `reserved4`      | **0**; non-zero reject                                      |

<!-- markdownlint-enable MD013 -->

**Reserved fields** (`reserved2`, `reserved4`) and **all flag bits** MUST be
zero until RMP v1 negotiates new semantics.

### 1.2 Field requirements

- `magic` / `header_version` / `header_len` are validated before parsing the
  rest of the header; a truncated 64-byte header raises **TruncatedHeader**.
- `schema_id` identifies a **primitive family** (Observation, Intent, Artifact,
  ToolResult, Critique, StateDelta, ErrorReport, etc.). The registry lives in
  [`docs/rmp-registry.md`](rmp-registry.md).
- `body_len` MUST match the actual MsgPack payload length and participates in
  the framing equality checks below.
- `trace_id` + `msg_id` are used for dedupe and telemetry.

## 2. Body envelope (MsgPack map)

Body bytes are encoded as a MsgPack map of the form:

```jsonc
{ "type": "<family.kind.vN>", "payload": <object|bytes>, "meta"?: <map> }
```

- The **header** `schema_id` selects the primitive family (Observation, Intent,
  Artifact, ToolResult, Critique, StateDelta, ErrorReport, etc.).
- The `"type"` string is the canonical registry entry (e.g.,
  `"artifact.created.v1"`, `"error.report.v1"`) and MUST parse as
  `<family>.<kind>.<version>` where the family prefix matches the primitive.
- Receivers MUST cross-check `schema_id` with the body type; mismatches raise
  **BodyTypeMismatch**.
- `meta` is optional and carries diagnostics (`opening_id`, `priority`, tags,
  budgeting hints). Unknown keys MUST be ignored. **`opening_id` lives in
  `meta`, not the fixed header**, in v0.

## 3. Framing invariants

- `frame_len` **MUST equal** `header_len + body_len`. Any mismatch yields
  **LengthMismatch** and the frame is dropped.
- `header_len` is fixed at 64 bytes in v0. Future versions MUST bump
  `header_version` and, if necessary, `header_len`.
- Implementations MAY cache `header_len`, but they MUST still compare the actual
  field to `64` and reject anything else.

## 4. TTL & expiry handling

- Compute `expires_at_ms = created_at_ms + ttl_ms` using **u128 arithmetic**.
- If `ttl_ms == 0`, raise **InvalidTtl** before any delivery attempts.
- If the addition overflows `u128` or the resulting value does not fit in `u64`,
  raise **InvalidExpiry**.
- Receivers MUST drop frames once `now_ms >= expires_at_ms`, emitting
  **Expired** in counters/telemetry. Drops still publish the body to
  `rlp/sys/drops` (see below) so operators can inspect failure causes.

## 5. Duplicate suppression & drop telemetry

- Dedupe scope: **per (topic + subscriber instance)**.
- Key: `(trace_id, msg_id)` tracked in a bounded LRU whose max-age is at least
  the configured max TTL.
- Dropping a frame for **TTL**, **Duplicate**, or **back-pressure** MUST:
  1. Increment `drops_total{reason=...}` metrics.
  2. Publish a structured event on `rlp/sys/drops` with
     `{reason, topic, trace_id, msg_id, expires_at_ms?}`. Emitters MUST
     rate-limit this topic to avoid storms.

## 6. Limits & safety

- **Max `body_len`:** configurable, default **8 MiB**. Larger frames are
  rejected with **BodyTooLarge** before touching MsgPack state.
- **Unknown `schema_id`:** reject with **UnknownSchema**.
- **Unsupported `header_version`:** reject with **UnsupportedVersion**.
- **Reserved fields / flags:** reject non-zero values with
  **InvalidHeaderFlags**.
- **Body decoding:** MsgPack parse failures raise **BodyDecodeError**.

## 7. Error taxonomy (test oracle)

<!-- markdownlint-disable MD013 -->

| Error                | One-liner                                                             |
| -------------------- | --------------------------------------------------------------------- |
| `InvalidMagic`       | `magic` bytes were not `"RMP0"`.                                      |
| `UnsupportedVersion` | `header_version` or `header_len` differed from `0/64`.                |
| `TruncatedHeader`    | Fewer than 64 header bytes were available.                            |
| `InvalidHeaderFlags` | Non-zero `flags`, `reserved2`, or `reserved4` encountered in v0.      |
| `LengthMismatch`     | `frame_len != header_len + body_len`.                                 |
| `UnknownSchema`      | `schema_id` not present in the registry.                              |
| `BodyTooLarge`       | `body_len` exceeded the configured limit.                             |
| `InvalidTtl`         | `ttl_ms` was zero.                                                    |
| `InvalidExpiry`      | `created_at_ms + ttl_ms` overflowed u128 or could not fit in u64.     |
| `Expired`            | `now_ms >= expires_at_ms` at receipt time.                            |
| `Duplicate`          | `(trace_id, msg_id)` already seen within the dedupe horizon.          |
| `BodyDecodeError`    | MsgPack body failed to parse according to the schema.                 |
| `BodyTypeMismatch`   | Body `"type"` string did not match the family implied by `schema_id`. |

<!-- markdownlint-enable MD013 -->

Test suites MUST pin at least one fixture for each entry above.

## 8. Example: frame hex dump

A minimal `ErrorReport` frame with `schema_id = 0x000A` and body:

```json
{
  "type": "error.report.v1",
  "payload": {
    "code": "tool.unavailable",
    "message": "mailer offline"
  },
  "meta": {
    "opening_id": 1234
  }
}
```

```text
0000: 00 00 00 a0 52 4d 50 30 00 00 00 40 00 00 00 00
0010: 00 0a 00 00 00 00 00 60 00 00 01 93 23 64 5c 7b
0020: 00 00 00 00 00 00 ea 60 11 22 33 44 55 66 77 88
0030: 99 aa bb cc dd ee ff 00 00 00 00 00 00 00 00 2a
0040: 00 00 00 00 83 a4 74 79 70 65 af 65 72 72 6f 72
0050: 2e 72 65 70 6f 72 74 2e 76 31 a7 70 61 79 6c 6f
0060: 61 64 82 a4 63 6f 64 65 b0 74 6f 6f 6c 2e 75 6e
0070: 61 76 61 69 6c 61 62 6c 65 a7 6d 65 73 73 61 67
0080: 65 ae 6d 61 69 6c 65 72 20 6f 66 66 6c 69 6e 65
0090: a4 6d 65 74 61 81 aa 6f 70 65 6e 69 6e 67 5f 69
00a0: 64 cd 04 d2
```

- Bytes `0000–0003`: `frame_len = 0x000000a0 = 160` (`64 + 96`).
- Bytes `0004–003f`: fixed header (magic, version, len, flags, schema, body
  length, timestamps, TTL, IDs, reserved zeros).
- Bytes `0040–00a3`: MsgPack body (three-entry map with `meta.opening_id`).

Use this vector as a golden test for codecs; decoding should produce the header
fields above and the stated body map exactly.

## 9. Control plane (MVP)

The control plane uses the same bus and framing:

- Topic: `rlp/ctrl`.
- Submit: `CT_CTRL_REQ` with
  `ControlRequest::RunSubmit { request_id, opening_yaml }`.
  - The CLI applies any `--params` override and sends the merged YAML as
    `opening_yaml`.
  - The frame `header.trace_id` MUST equal `request_id` for correlation.
  - `ttl_ms = 30000` (30s) for control requests.
- Response: `CT_CTRL_RESP` with
  `ControlResponse::{RunAccepted, RunRejected, RunCancelled}`.
- `RunAccepted` carries `{ request_id, trace_id, opening_id, opening_name }`.
- Idempotency: the daemon MUST treat duplicate `CT_CTRL_REQ` with identical
  `trace_id` as idempotent.
- Acceptance timeout: the CLI waits up to 2000 ms for `RunAccepted` before
  failing with guidance to start the daemon or use `--local`.
- Rejections: `RunRejected` includes a human `reason`. Future protocol revisions
  may add structured `code` / `detail` fields once the daemon emits them; the
  CLI surfaces the textual reason today.

### 9.1 Executor ↔ agent envelopes

- Topic namespace: `agent/<agent_ref>` (one topic per logical agent). The
  executor publishes requests here. Each request includes a unique `reply_topic`
  (e.g., `rlp/runs/<trace_id>/agents/<agent>/<uuid>`), and the agent publishes
  its response on that topic.
- Request (`CT_EXECUTOR_AGENT_REQUEST`, type `intent.executor.agent.request.v1`)
  body:

  ```json
  {
    "type": "intent.executor.agent.request.v1",
    "payload": {
      "node": {
        /* serialized Node from the DSL */
      },
      "inputs": {
        /* NodeInputs */
      },
      "trace_id": "trace:...",
      "opening_id": "opening:...",
      "attempt": 1,
      "reply_topic": "rlp/runs/<trace>/agents/<agent>/<uuid>"
    },
    "meta": null
  }
  ```

  RMP headers carry `trace_id`, `msg_id`, and TTL (default **30s**). The
  executor subscribes to `reply_topic` before publishing the request.

- Response (`CT_EXECUTOR_AGENT_RESPONSE`, type
  `toolresult.executor.agent.response.v1`) body:

  ```json
  {
    "type": "toolresult.executor.agent.response.v1",
    "payload": {
      "Completed": {
        "ports": {
          "out": [
            {
              "schema_id": 0x00D1,
              "type": "artifact.draft.email.v1",
              "value": { ... }
            }
          ]
        }
      }
    }
    // meta is currently omitted
  }
  ```

  or

  ```json
  {
    "type": "toolresult.executor.agent.response.v1",
    "payload": {
      "Failed": { "retryable": false, "reason": "timeout" }
    }
  }
  ```

- Agents (WASM or shim) consume the request body, execute their work, and
  publish the response on the per-request `reply_topic`. The executor rebuilds
  `NodeOutputs` from the typed port data.

## 10. Run event stream (MVP)

After `RunAccepted`, the daemon publishes a single unified stream of run events:

- Topic: `rlp/runs/<trace_id>/events`.
- Schema: `CT_RUN_EVENT`.
- Payload shape:

```json
{
  "kind": "log|status|plan|trace|artifact|progress",
  "level": "info|warn|error"?,
  "message": "...",
  "meta": {
    "ts_ms": <u64>,
    "run_id": "...",
    "node_id": "..."?,
    "span_id": "..."?
  }
}
```

- Ordering: FIFO per publisher per topic; no sequence number in MVP.
- Live-only: no backfill on subscribe; the daemon persists durable history in
  the KB.

## 11. Topic namespaces & ACLs

- `rlp/ctrl` — control-plane submit/ack.
- `rlp/runs/<trace_id>/events` — unified per-run event stream.
- `rlp/sys/*` — reserved for system/admin (drops, metrics, stats). Drop notices
  use `CT_BUS_DROP_NOTICE` on `rlp/sys/drops`.
- `action.decision` (schema `CT_ACTION_DECISION`) may only be published by
  `ui|tui` publisher kinds; the bus rejects other publishers.

## 12. Socket & transport (MVP)

- Single Unix domain socket is used for both bus and control.
- Discovery precedence:
  1. If `runtime.socket_path` is non-empty, use it (short‑circuit; if
     unreachable → error immediately).
  2. Else if `runtime.sockets_dir` is set, `${runtime.sockets_dir}/rmp.sock`.
  3. Else `~/.runloop/sock/rmp.sock`.
  4. Else `/run/runloop/rmp.sock`.
