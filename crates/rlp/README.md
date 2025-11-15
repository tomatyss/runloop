# rlp CLI

`rlp` is the terminal client for Runloop. It routes openings to the daemon,
prints live `RunEvent` streams, and exposes quick inspection utilities for the
router, knowledge base, and configuration layers.

## Commands

### `rlp run`

```bash
rlp run <opening.yaml> [--params '{"key":"value"}'] [--trace-out trace.json] [--local]
```

- **Daemon first:** the CLI probes sockets in this order: `runtime.socket_path`,
  `${runtime.sockets_dir}/rmp.sock`, `~/.runloop/sock/rmp.sock`,
  `/run/runloop/rmp.sock`. If `runtime.socket_path` is set but unreachable, it
  errors immediately. If none are serving, it exits with a hint to start
  `runloopd` or re-run with `--local`.
- **`--local`** runs inline via the embedded executor. When connected to the
  daemon, the CLI submits the opening over the bus and streams `RunEvent` v1
  records from `rlp/runs/<trace_id>/events`.
- **NDJSON output:** stdout receives one JSON object per line in the order
  `run.started → node.* → run.finished`. Each record contains `ts_ms`,
  `trace_id`, `run_id`, `opening_id`, `kind`, `level`, `message`, and a `meta`
  object with kind-specific fields (`params`, `node`, `chunk`, `status`,
  `duration_ms`, etc.).
- **`--trace-out`** writes the `RunTrace` produced by the executor (daemon trace
  export TBD). Successful runs persist `run.started` / `run.finished` into the
  KB even when executed locally, so later replay/debug tooling can target the
  same IDs.

Example NDJSON slice:

<!-- markdownlint-disable MD013 -->

```json
{"ts_ms":1731174100123,"trace_id":"trace:...","run_id":"trace:...","opening_id":"opening:...","kind":"run.started","level":"info","message":"run started","meta":{"params":{"topic":"Q4"}}}
{"ts_ms":1731174100456,"trace_id":"trace:...","run_id":"trace:...","opening_id":"opening:...","kind":"node.started","level":"info","message":"node contacts started","meta":{"node":"contacts","attempt":1}}
{"ts_ms":1731174105321,"trace_id":"trace:...","run_id":"trace:...","opening_id":"opening:...","kind":"run.finished","level":"info","message":"run ok","meta":{"status":"ok","duration_ms":5201}}
```

<!-- markdownlint-enable MD013 -->

### `rlp why "<prompt>"`

Classifies a prompt via the router. Output defaults to a table when stdout is a
TTY, otherwise JSON. Override with `--json` / `--table` and tune `--max-cols`,
`--max-rows`, or `--no-wrap` as needed.

### `rlp kb query`

```bash
rlp kb query <SQL...> [--json|--table]
```

Runs read-only SQL against the KB views. Results stream through the same
renderer used by `why`, preserving column widths and wrapping long cells. Use
`--json` to obtain the raw `QueryResult` payload.

### `rlp kb why <entity>`

Prints a provenance ladder for any `entity_history` key (runs, contacts,
artifacts, etc.). The default table lists
`ts_ms | event | kind | actor | scope | summary`; `--json` returns the canonical
`EventRecord` list. A future `--resolve` flag will hydrate linked
artifacts/contacts inline.

### `rlp config path`

```bash
rlp config path [--all] [--json]
```

- Without flags, prints the highest-precedence config file path (or a note if
  only defaults/env were applied).
- `--all` shows the entire provenance chain as a table: defaults, each file
  (with load status), and the environment layer. Overlay notes describe every
  key that changed at each layer.
- `--json` emits `{sources: [...], overrides: [...], resolved: {...}}` for
  consumption by tooling.

### `rlp kb search` / `rlp kb migrate`

`kb search` remains JSON-only today; `kb migrate` is unchanged and prints
progress as before.

## Output Controls at a Glance

| Flag         | Scope                                               |
| ------------ | --------------------------------------------------- |
| `--json`     | Forces JSON output (TTY or pipe).                   |
| `--table`    | Forces table output even when piping.               |
| `--max-cols` | Truncates table columns (ellipsizes headers/cells). |
| `--max-rows` | Limits rows shown; adds a continuation note.        |
| `--no-wrap`  | Disables soft-wrapping; truncates with `…` instead. |

The renderer automatically paginates interactive tables (`stdin` + `stdout` both
TTY) with a simple “`-- more --`” prompt.

## Known Gaps

- `rlp replay <trace_id>` still targets explicit trace files; the KB lookup path
  will land alongside daemon-backed trace storage.
- Parameter schema validation relies on future agent manifest metadata.
