# agtop TUI

`agtop` is a terminal monitor for Runloop openings. It can replay NDJSON run
events from stdin/files or connect live to the daemon bus to stream run events,
metrics, and action confirmations.

## Usage

```bash
# Replay NDJSON (stdin or file)
agtop -i trace.ndjson

# Live mode (connect to runloopd bus)
agtop --connect --trace-id <trace> [--socket /path/to/rmp.sock] \
      [--monitor-agents agent_a,agent_b]

# Keys
# Tab / Shift+Tab: cycle panes
# q / Ctrl+C: quit
# ?: help overlay
# /: filter within active pane
# .: pause/resume (events buffer while paused)
# !: clear active pane
# Enter/Esc: approve/reject action proposal when confirmation modal is shown
```

## Panes

- **Log** – streaming log lines from run events.
- **Plan** – per-node status/attempt/duration plus dependency counts (in/out).
- **Metrics** – system metrics from `rlp/sys/metrics` and per-agent metrics from
  `rlp/agents/<agent>/metrics` (requires `--monitor-agents`).
- **Trace** – ladder-style trace entries.

## Status bar

Shows mode (LIVE/REPLAY), trace id, agents_running and bus_queue_depth (when
present in system metrics), current pane, pause/filter indicators, and
confirmation badge when an action proposal is pending.

## Action confirmations

`agtop` subscribes to `action.request`; approvals/rejections publish
`action.decision` with correlation to the proposal’s `msg_id`. Only UI/TUI
publisher kind is used.

## Notes

- Per-agent metrics require listing agent IDs with `--monitor-agents` until
  auto-discovery lands.
- Pause (`.`) buffers incoming events/metrics and applies them on resume.
