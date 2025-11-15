# Openings DSL

> **Status:** v0 (draft). Defines the declarative language for composing agents
> into runnable DAGs (“Openings”) with replay and budgets. Normative behaviors
> (execution, bus, TTL, audit) align with the Protocol, Runtime, and Roadmap
> docs.

---

## 1) What is an Opening?

An **Opening** is a typed, declarative **DAG** of agents (“nodes”) and their
connections (“edges”). It is the plan the runtime executes: fan‑out/fan‑in,
retries, timeouts, budgets, and deterministic **replay** are first‑class. The
bus carries schema‑tagged messages (RMP), and every crossing is traced and
auditable.

**MVP reference opening (compose_email):**
`contact_resolver → context_gatherer → writer → critic → mailer (human-confirm)`;
used throughout examples. The canonical YAML lives at
`examples/openings/compose_email.yaml`.

---

## 2) Goals & non‑goals

### Goals

- Human‑readable spec for orchestrating agents.
- Deterministic **IR** (intermediate representation) suitable for validation,
  execution, and replay.
- Minimal templating; no Turing‑complete logic.

### Non‑goals (v0)

- Distributed execution (single machine only).
- Rich conditionals/loops; stick to edge predicates and retries.

---

## 3) Two surfaces: YAML (canonical) and a “text sugar”

- **Canonical:** **YAML → IR** parser in `crates/openings/`. All validation and
  runtime semantics reference the YAML form.
- **Sugar:** an optional brace‑style snippet mirrors README examples and
  compiles to the same IR; it is syntactic sugar only (non‑normative).

> **Consistency note.** README shows a brace DSL, while the implementation plan
> standardizes on **YAML → IR**. This spec makes YAML normative and documents
> the brace form as optional sugar to keep examples approachable.

---

## 4) Quick start

### 4.1 Minimal YAML opening

```yaml
version: 0
name: compose_email
goals:
  - "send a concise email to john about Q4 plan"
params:
  recipient: "john"
  topic: "Q4 plan"

policy:
  budget_tokens: 8000 # overall model budget hint
  timeout_ms: 30000 # opening-level wall time
  confirm_external: true # force human confirm on external actions

nodes:
  - id: contacts
    use: agent:contact_resolver
    with: { query: "{{params.recipient}}" }
    retry: { max_attempts: 2, backoff_ms: 2500 }
    timeout_ms: 5000

  - id: context
    use: agent:context_gatherer
    with: { topic: "{{params.topic}}" }

  - id: draft
    use: agent:writer
    with:
      model: "mixtral-8x7b"
      topic: "{{params.topic}}"
      tone: "neutral-friendly"
    timeout_ms: 15000
    budget_tokens: 4000

  - id: review
    use: agent:critic

  - id: send
    use: agent:mailer
    with:
      require_human_confirm: true
      topic: "{{params.topic}}"

edges:
  - from: contacts.out # port on 'contacts'
    to: draft.recipients
  - from: contacts.out
    to: context.contact
  - from: context.out
    to: draft.context
  - from: draft.out
    to: review.in
  - from: draft.out
    to: send.draft
  - from: review.review
    to: send.review
  - from: contacts.out
    to: send.contact
  - from: review.ok==true # simple predicate on a boolean output
    to: send.in

success:
  any_of:
    - review.ok == true
artifacts:
  save:
    - draft.out
```

- **Templating:** only `{{params.*}}` substitution; no loops/expressions in
  templates. Use simple edge predicates for control.

### 4.2 Equivalent “text sugar” (non‑normative)

```text
opening "compose_email" {
  goals: ["email to john about q4 plan"]
  nodes:
    contacts := agent("contact_resolver", query="{{params.recipient}}")
    context  := agent("context_gatherer", topic="{{params.topic}}")
    draft    := agent("writer", model="mixtral-8x7b", topic="{{params.topic}}", tone="neutral-friendly")
    review   := agent("critic")
    send     := agent("mailer", require_human_confirm=true, topic="{{params.topic}}")
  edges:
    contacts.out -> draft.recipients
    contacts.out -> context.contact
    context.out  -> draft.context
    draft.out    -> review.in
    draft.out    -> send.draft
    review.review -> send.review
    contacts.out -> send.contact
    review.ok==true -> send.in
}
```

This compiles to the same IR as the YAML.

---

## 5) Language reference (YAML)

### 5.1 Top‑level keys

| Key                       | Type        | Required | Meaning                                                    |
| ------------------------- | ----------- | :------: | ---------------------------------------------------------- |
| `version`                 | int         |    ✓     | DSL version (this doc = 0).                                |
| `name`                    | string      |    ✓     | Opening identifier (DNS‑label recommended).                |
| `goals`                   | [string]    |    –     | Human intent; logged with trace.                           |
| `params`                  | map         |    –     | Parameter bag available to templates.                      |
| `policy.budget_tokens`    | int         |    –     | Aggregate budget hint propagated to nodes/broker.          |
| `policy.timeout_ms`       | int         |    –     | Opening‑level wall‑clock timeout; cancels remaining nodes. |
| `policy.confirm_external` | bool        |    –     | Require human confirmation for send/delete/spend actions.  |
| `nodes`                   | [Node]      |    ✓     | Agent or nested opening invocations.                       |
| `edges`                   | [Edge]      |    ✓     | Connections between node **ports**.                        |
| `success`                 | SuccessExpr |    –     | Completion condition.                                      |
| `artifacts.save`          | [PortRef]   |    –     | Ports whose outputs should be persisted in KB.             |

### 5.2 Node

```yaml
- id: string                      # unique within opening
  use: "agent:<name>" | "opening:<name>"
  with: { ... }                   # agent config / args
  retry: { max_attempts: 0..N, backoff_ms: 0.. }    # default: 0
  timeout_ms: 0..                 # default: none (but see §8 safety)
  budget_tokens: 0..              # overrides opening budget for this node
  tags: [string]                  # arbitrary labels for metrics/filtering
```

- `use` selects either an agent bundle (WASM under runtime) or a nested opening
  (composition).
- `with` is passed as the **payload** of the node’s initial **Intent** message;
  the node decides how to interpret it.

### 5.3 Ports & edges

```yaml
- from: "<node>.<port>[?predicate]"
  to: "<node>.<port>"
```

- Every node exposes named **output ports** and **input ports**. The SDK
  recommends defaults (`in`, `out`, `ok`, `err`) but agents may define richer
  port schemas. Fan‑out is N edges from one output; fan‑in blocks until all
  upstreams are satisfied.
- Predicates are simple comparisons on boolean or enum outputs (e.g.,
  `review.ok==true`). Complex control belongs in the agent, not the DSL.

### 5.4 Success expressions

```yaml
success:
  all_of: ["review.ok == true", "exists(draft.out)"] # OR
# success:
#   any_of: ["review.ok == true"]
```

On timeout/failure with no satisfied success condition, the opening fails.

### 5.5 Parameters & templating

- Only string replacement of `{{params.*}}` in `with` maps. No arithmetic, no
  conditionals. This avoids making the DSL a programming language.

---

## 6) Execution semantics

1. **Parse & validate** YAML → **IR**, producing helpful errors with
   line/column.
2. **Topological scheduling.** Nodes run when all inbound dependencies are
   ready. Fan‑in edges cause waits; fan‑out clones messages/artifacts.
3. **Runtime & bus.** Node work is enacted by sending/receiving **RMP** messages
   over the local UDS bus. The fixed header carries `trace_id`, `msg_id`,
   `schema_id`, `created_at_ms`, and `ttl_ms`; `opening_id` and other metadata
   ride in the MsgPack envelope `{ type, payload, meta? }`.
4. **TTL & duplicates.** Receivers enforce TTL using `created_at_ms + ttl_ms`;
   duplicates are ignored using an LRU dedupe cache keyed by
   `(trace_id,msg_id)`. Drops increment counters and may be broadcast on
   `rlp/sys/drops`.
5. **Retries & timeouts.** Per‑node `retry` and `timeout_ms` govern backoff and
   cancellation. Opening‑level `policy.timeout_ms` cancels remaining nodes.
6. **Replay.** The engine records input/output message IDs per node;
   `rlp replay <trace_id>` re‑feeds inputs to reproduce identical outputs (given
   deterministic providers). Diff tooling highlights mismatches.
7. **Audit & KB events.** Open/close of a run emits
   `run.started`/`run.finished`; agent actions may emit `cap.audit` on denied
   ops per policy.

---

## 7) Types & schemas on the wire

RMP **schema_id** is a `u16` referencing a small registry of content types
(e.g., `CT_OBSERVATION=1`, `CT_INTENT=2`, `CT_ARTIFACT=5`, `CT_STATE_DELTA=7`).
Message bodies are MsgPack with JSON‑Schema definitions for validation and
evolution.

> **Envelope.** The `{ type, payload }` MsgPack map is the default body shape;
> higher‑level helpers exist in `crates/rmp/` to encode/decode and stamp TTL.

---

## 8) Safety, capabilities, and confirmation

- Agents run as **WASM/WASI** sandboxes under the Runtime with least‑privilege
  **capabilities**. File system, network, time, KB, model, secrets, and exec are
  checked by hostcalls; **deny‑by‑default**.
- **External actions** (send/delete/spend) must be **human‑confirmed** unless
  policy explicitly allows. The mailer example keeps “dry‑run + confirm” for
  MVP.
- **TTL defaults.** To avoid “immortal” frames, DSL v0 **requires** explicit
  `ttl_ms` in RMP headers at encode time; the engine rejects zero TTL during
  publication. (Runtime’s bus enforces TTL regardless.)

---

## 9) Observability & metrics

- **Tracing.** Each crossing creates a span with `trace_id` propagation;
  `runloop trace <id>` renders a ladder diagram.
- **Metrics.** Counters for sent/received/dropped, cap_denied, broker_calls,
  cache_hits; gauges for agents_running, rss_total, bus_queue_depth; surfaced in
  `agtop`.
- **Drops.** Receivers may publish structured drop notifications to
  `rlp/sys/drops`; failure totals are also readable via metrics.

---

## 10) CLI integration

- Run:
  `rlp run examples/openings/compose_email.yaml --params '{"recipient":"john","topic":"Q4 plan"}' --trace-out trace.json`
  - Executes the YAML plan via the openings engine, drives the canonical crew
    (contact resolver → context gatherer → writer → critic → mailer), prints
    node status, and writes an optional JSON trace for later replay. You need a
    writable KB path plus a configured model broker; if no provider is reachable
    the writer falls back to the heuristic template. The CLI secret resolver
    reads broker credentials from the environment, and mail send still requires
    human confirmation unless you opt out in
    `security.confirm_external_actions`.
- Explain routing: `rlp why "<prompt>"`
- Replay: `rlp replay trace.json --opening examples/openings/compose_email.yaml`
  - KB: `rlp kb query ...`, `rlp kb why <id>` All commands produce structured
    output with sensible exit codes.

---

## 11) Validation rules (normative)

1. **Graph soundness:** nodes are unique; edges reference existing ports; no
   cycles.
2. **Port compatibility:** serialized payload types must match the consumer
   input schema_id (static lint + runtime check).
3. **Budgets:** non‑negative; per‑node budget must not exceed opening budget
   (lint).
4. **Timeouts:** if absent at node level, the runner inherits opening timeout;
   long‑running agents must explicitly opt into higher timeouts.
5. **Templating:** only `{{params.*}}` is allowed; unresolved placeholders are
   errors.
6. **Security:** if `confirm_external=true`, any node with
   `with.require_human_confirm=false` is rejected (cannot downgrade global
   policy).

---

## 12) Examples

### 12.1 Compose email (canonical)

`examples/openings/compose_email.yaml` (identical to §4.1) is the reference
artifact used by MVP acceptance.

### 12.2 Summarize logs (fan‑out/fan‑in)

```yaml
version: 0
name: summarize_logs
nodes:
  - id: gather1
    use: agent:context_gatherer
    with: { topic: "service:A" }
  - id: gather2
    use: agent:context_gatherer
    with: { topic: "service:B" }
  - id: merge
    use: agent:writer
edges:
  - { from: gather1.out, to: merge.in }
  - { from: gather2.out, to: merge.in }
success:
  any_of: ["exists(merge.out)"]
```

---

## 13) File locations & packaging

- **Openings** live under `examples/openings/` in‑repo; installed bundles may
  ship curated openings with their agents.
- OS packages place state under `/var/lib/runloop` (system mode) and the Runloop
  Message Protocol socket under `/run/runloop/rmp.sock`; user mode uses
  `~/.runloop/sock/rmp.sock`.

---

## 14) Alignment with Protocol, Runtime, KB

- **Protocol (RMP).** Fixed‑size header (incl. `created_at_ms`, `ttl_ms`, IDs) +
  MsgPack envelope; TTL and duplicate rules apply to all frames emitted by
  openings.
- **Runtime.** Agents are wasm32‑wasi; capability gates around
  FS/NET/TIME/KB/MODEL/SECRETS/EXEC; audit events for denied ops.
- **KB (POG).** Runs emit `run.started/finished`; artifacts saved via
  `artifacts.save` are materialized and traceable via `kb.why`.

---

## 15) EBNF (YAML schema projection)

> YAML is the source of truth; this EBNF describes the **IR** structure after
> parsing.

```ebnf
Opening        := { version: Int, name: Ident, goals?: [Str], params?: Map,
                    policy?: Policy, nodes: [Node], edges: [Edge],
                    success?: Success, artifacts?: Artifacts }

Policy         := { budget_tokens?: Int, timeout_ms?: Int, confirm_external?: Bool }

Node           := { id: Ident, use: Use, with?: Map, retry?: Retry,
                    timeout_ms?: Int, budget_tokens?: Int, tags?: [Ident] }

Use            := "agent:" Ident | "opening:" Ident

Retry          := { max_attempts: Int, backoff_ms: Int }

Edge           := { from: PortRef, to: PortRef }
PortRef        := Ident "." Ident [ "==" (Bool | Int | Str) ]   # simple predicate suffix

Success        := { any_of?: [Expr] } | { all_of?: [Expr] }
Expr           := Ident "." Ident "==" (Bool | Int | Str) | "exists(" PortRef ")"

Artifacts      := { save?: [PortRef] }

Ident          := /[a-z][a-z0-9_]{0,63}/
```

---

## 16) Known mismatches & decisions

- **YAML vs brace DSL:** YAML is **normative**; brace DSL is sugar compiled to
  IR. (README examples remain valid as sugar.)
- **RMP header fields:** This doc assumes the header includes `created_at_ms`,
  `ttl_ms`, fixed header version/length, and IDs per Protocol/TODO. If the codec
  stub diverges, **Protocol drives**; update the runner accordingly.
- **TTL `0` semantics:** The DSL validator **rejects** 0 to avoid immortal
  frames. Bus/runtime still enforce TTL per Protocol. If a “no‑expire” policy is
  required later, we will add an explicit `ttl_ms: never` sentinel with clear
  safeguards.
- **Drop notifications:** Receivers may auto‑publish to `rlp/sys/drops`, but
  counters are always available; the daemon controls emission volume.

---

## 17) Tooling

- **CLI:** `rlp run`, `rlp why`, `rlp replay`, `rlp kb`.\*
- **TUI:** `agtop` shows Plan, Log, agtop metrics, Trace ladder.
- **Repo hygiene:** place openings in `examples/openings/`, keep examples
  compiling in tests, and document agent caps alongside updated openings.

---

## 18) Appendix — Reference IDs & content types

Core IDs (`TraceId`, `OpeningId`, `MsgId`) and the **content type** registry
(`CT_INTENT`, `CT_ARTIFACT`, etc., `u16`) are defined in `crates/core/`. Use
these when authoring schemas and mapping ports.

---

### Change log

- **v0:** Initial spec (YAML normative, brace sugar optional), edges with simple
  predicates, retries/timeouts/budgets, KB artifacts, TTL rules, drop
  notifications, and replay semantics.

---

### References

- README (concepts, sample opening), Roadmap (acceptance criteria), TODO (epics
  for RMP/Bus/Openings), Protocol/Runtime design notes (RMP headers, capability
  enforcement), KB design (events, artifacts, `kb.why`).

---

_See also: `docs/message-protocol.md` and `docs/kb-schemas.md` for schema IDs
and KB event kinds referenced here._
