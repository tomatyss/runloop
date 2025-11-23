# Terminal UI

> **Status:** In implementation for Epic I (CLI/TUI). This document captures the
> canonical UX/telemetry contract so `agtop` and the daemon evolve in lock-step.

## Overview

`agtop` is a ratatui-based monitor that subscribes to the Runloop bus for live
openings (or replays NDJSON from stdin/files). It presents four panes plus a
status bar:

1. **Log** – streaming log lines (from unified `CT_RUN_EVENT` stream).
2. **Plan** – DAG state (node status, attempts) derived from `RunEvent` nodes in
   the unified stream.
3. **agtop Metrics** – per-agent CPU/RSS/tokens and bus health aggregated from
   `rlp/sys/metrics` + `rlp/agents/<agent_id>/metrics`.
4. **Trace** – ladder view of crossings (from `trace` events in the unified
   stream).

Artifacts remain out of scope for Epic I and will follow in the Observability
epic.

## Layout snapshots (ASCII)

### Live run (streaming)

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ agtop • mode:user • opening:compose_email (trace 63f1…) • pane:Log • tokens   │
│ agents_running:3 • bus_queue_depth:1 • CONFIRM STATUS: clear                  │
├───────────────────────────────┬──────────────────────────────────────────────┤
│ LOG (rlp/runs/<id>/events)    │ PLAN (DAG from run events)                    │
│ [12:01:03] writer ▸ draft ok  │ contacts ✔  context ✔  draft ✱  review …      │
│ [12:01:05] critic warn retry  │ edges animate attempts / retries              │
│ …                             │                                               │
├───────────────────────────────┼──────────────────────────────────────────────┤
│ agtop metrics (rlp/sys/*)     │ TRACE (rlp/runs/<id>/events kind=trace)       │
│ agents_running:3  rss:420MB   │ [12:01:02] CLI → router → contact_resolver    │
│ msgs_sent:134 drops:0         │ [12:01:04] writer → critic                    │
│ bus_queue_depth:1 cache_hits:4│ … ladder scroll                               │
└───────────────────────────────┴──────────────────────────────────────────────┘
```

Keyed areas:

- Status bar mirrors the “Status Bar” contract and surfaces confirm state.
- Log/Plan panes share the unified stream; left = textual events, right = DAG
  view.
- Metrics pane aggregates global/per-agent gauges; Trace pane renders the
  ladder.

### Confirmation prompt visible

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ … CONFIRM STATUS: PENDING (action.proposal 27ac…)                            │
├───────────────────────────────┬──────────────────────────────────────────────┤
│ LOG …                         │ PLAN …                                       │
├───────────────────────────────┴──────────────────────────────────────────────┤
│                          CONFIRMATION REQUIRED                               │
│  Agent mailer proposed sending draft v3 to `john@example.com`                │
│  Enter=approve • Esc=reject • (.) pause stream • [?] help                    │
│  meta{opening_id:draft.send, ttl_ms:15000, trace_id:63f1…}                   │
├───────────────────────────────┬──────────────────────────────────────────────┤
│ agtop metrics …              │ TRACE …                                       │
└───────────────────────────────┴──────────────────────────────────────────────┘
```

When a confirmation dialog is focused the log/plan panes keep buffering events,
but input focus moves to the overlay; decisions publish `action.decision` via
the UI/TUI capability described in “Confirmations”.

## Status Bar

Always-on, single line:

- **Mode:** `user` vs `system` (from config).
- **Opening:** name + `trace_id`.
- **Pane:** active pane short name.
- **Tokens/Health:** summarized token burn, daemon pressure, and
  `agents_running` gauge.
- **Confirm Status:** shows `CONFIRM REQUIRED` when a pending `action.proposal`
  is awaiting a decision.

## Keybinds & Controls

`agtop` is keyboard-only. Defaults:

- `Tab` / `Shift+Tab` – cycle panes forward/backward.
- `q` – quit.
- `?` – help overlay (lists panes, keys, topic names).
- `/` – filter/search within the active pane (e.g., log substring, node id).
- `.` – pause/resume live updates (events buffer while paused and apply on
  resume).
- `!` – clear active pane buffer.
- `Enter` – when a confirmation prompt is focused, approve; `Esc` rejects
  (mirrors CLI confirmation semantics).

## Data Plane & Topics

- **Run submission:** CLI publishes `ControlRequest::RunSubmit` on `rlp/ctrl`.
  The daemon responds with `ControlResponse::RunAccepted` and begins emitting
  `CT_RUN_EVENT` records on `rlp/runs/<trace_id>/events`.
- **Unified stream:** `rlp/runs/<trace_id>/events` carries
  `{kind, level?, message, meta{ts_ms, run_id, node_id?, span_id?}}` events.
- **Metrics:** `rlp/sys/metrics` (global) + `rlp/agents/<agent_id>/metrics` (per
  agent). Minimum gauges/counters (Epic J prerequisites): `agents_running`,
  `rss_total`, `bus_queue_depth`, `msgs_sent`, `msgs_dropped`, `cap_denied`,
  `broker_calls`, `cache_hits`. Status bar surfaces `agents_running` and
  `bus_queue_depth` when present. Subscribe to per-agent metrics with
  `--monitor-agents` until auto-discovery is added.
- **Confirmations:** Agents emit `action.proposal`. Only publishers with kind
  `ui|tui` may send `action.decision`. For daemon-backed runs the CLI defers to
  the TUI (no inline prompt); `agtop` surfaces dialogs and publishes the
  decision.
- **Ladder (trace pane):** `rlp trace <id>` and the TUI consume ladder hops
  emitted on the run stream and persisted in `run.trace.ladder`. Each hop
  captures `{ts_ms, from, to, schema_id, frame_len, body_len, msg_id, kind}`.
  Live streams are merged with the persisted ladder; `--include-control` /
  `--include-drops` opt into control-plane and drop notices when debugging.

## UX Principles

- **Non-blocking panes:** pane switches (`Tab`) never pause other streams;
  pausing is explicit via `.`.
- **Structured layout:** each pane uses consistent column widths; tables fall
  back to JSON view with `--json` flag (shared between CLI & TUI for
  scripts/tests).
- **Error surfacing:** desyncs (e.g., missing metrics) render `N/A` with muted
  styling rather than stale values. Bus disconnects surface in the status bar
  with reconnect attempts.
- **Deterministic replay:** trace pane can load historical runs via
  `rlp replay <trace_id>` without contacting live agents (side-effect-free
  replayer).

## Accessibility & Theme

- Default theme `mono` (Config v1) uses high-contrast colors; no critical signal
  relies on color alone.
- All panes expose textual labels (e.g., `[WARN] critic timed out`) for screen
  readers.
- Keyboard-only interactions; focus indicators are double-underlined entries.
- Future enhancements (post-Epic I): configurable palettes and screen-reader
  mode.
