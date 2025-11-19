# Runloop — Roadmap (12 months to v1.0)

> **Doc status:** Living spec (acceptance metrics are normative).  
> **Last updated:** 2025‑11‑03

This roadmap is milestone‑driven with clear **deliverables**, **exit criteria**,
and **acceptance metrics**. Month counts are relative to project start (**M0 =
kickoff**). Tracks can run in parallel where dependencies allow.

---

## 0) Scope & principles (used for fast trade‑offs)

1. **Terminal‑first.** The TUI/CLI is the cockpit; everything is inspectable.
2. **Local‑first, cloud‑optional.** Works offline; sync/enrich when online.
3. **Small pieces, typed loosely.** Agents interoperate over a minimal typed bus
   (RMP).
4. **Least privilege.** Capabilities by default; human confirmation for external
   side effects.
5. **Deterministic replay.** All openings are replayable with provenance and
   budgets.
6. **Ship > perfect.** Debian first; keep clean seams for portability.

---

## 1) Staffing assumptions (minimal viable team)

- **Core**: 1 runtime lead, 1 infra/ops, 2 systems engineers (Rust), 1 ML/LLM
  engineer, 1 TUI/UX engineer, 1 PM/TPM, 1 security/priv reviewer (part‑time), 1
  QA/automation.
- **Optional**: 1 tech writer, 1 developer relations.

Governance & repo hygiene run in parallel (see Phase G below).

---

## 2) Artifact map (what “done” looks like at v1.0)

- **Daemons & CLIs:** `runloopd`, `rlp`, `agtop`.
- **Agent SDK (Rust/wasm32‑wasi)** with examples & docs.
- **RMP (Runloop Message Protocol) v0 (frozen)** with header table, framing
  example, and message registry (foundation for future v1 negotiations).
- **Openings DSL** with grammar (EBNF/ABNF), replay semantics, and examples.
- **POG (Personal Ops Graph)**: SQLite event log + materialized views + vector
  index; `kb.query`, `kb.why`, `kb.write_event`.
- **TUI** panes: Plan, Log, Artifacts, **agtop**, Trace.
- **Packaging:** Signed agent bundles; OS packages (`.deb`), ISO image for demo;
  upgrade/migration path.
- **Docs:** Quickstart, SDK guide, Opening cookbook, Security whitepaper, man
  pages.

---

## 3) Phased plan (months → milestones → exit gates)

### Phase 0 — Preflight & foundation (M0–M1)

**M0.1 — Charter & constraints**  
**Deliverables**

- Product brief v1, non‑goals, supported platforms (Debian 12; x86_64 + arm64).
- License decision (e.g., Apache‑2.0) and third‑party notice template.  
  **Exit criteria**
- Sign‑off on scope, principles, and release targets.

**M0.2 — Architecture & interfaces 0.1**  
**Deliverables**

- **RMP v0 freeze**: header/body schemas finalized (`magic="RMP0"`,
  `header_version=0`, `header_len=64`, reserved flags/words zeroed, TTL & dedupe
  rules, schema ↔ body kind cross-checks, MsgPack envelope).
- **Agent packaging 0.1**: `manifest.toml`, `policy.caps`, `tools.json`
  (documented in `docs/tool-attachments.md`), signing
  model.
- **Openings DSL 0.1**: grammar (EBNF) + replay semantics + examples.
- **POG data model 0.1**: entities (`Identity`, `Account`, `Contact`,
  `Artifact`, `Event`, `Policy`), **JCS JSON** for payload/provenance, **BLAKE3
  BLOB(32)** hashes.  
  **Exit criteria**
- RFCs merged; skeletal crates compile.

**M1.0 — Repo, build, CI**  
**Deliverables**

- Monorepo layout (`/runloopd`, `/cli`, `/tui`, `/sdk/agent-rust`, `/pog`,
  `/broker`, `/examples`).
- CI: fmt, clippy, unit tests, cross‑compile, nightly artifacts.  
  **Exit criteria**
- One‑command dev setup; CI green on two arches.

---

### Phase 1 — Minimal runnable system (M2)

**M2.0 — Runtime & sandbox MVP**  
**Deliverables**

- `runloopd` skeleton: process manager, config loader (schema v1), capability
  registry.
- Wasmtime (WASI) integration; launch `hello-agent.wasm`.
- Capability gates: FS (scoped), Net (off by default), Time, POG read.  
  **Exit criteria**
- Run a sample agent via CLI; controlled failure/exit codes observable.

**M2.1 — CLI & TUI skeleton**  
**Deliverables**

- `rlp` commands: `run`, `plan`, `trace <id>`, `cap grant`.
- TUI status bar + panes (Plan, Log); non‑blocking rendering.  
  **Exit criteria**
- Route a prompt to a static agent; watch execution in TUI.

**M2.2 — POG storage 0.1**  
**Deliverables**

- Append‑only event log (SQLite) + materialized view service.
- APIs: `kb.query`, `kb.why`, `kb.write_event`.  
  **Exit criteria**
- Insert `contact.upserted`, retrieve with provenance via `kb.why`.

---

### Phase 2 — Router & model broker (M3)

**M3.0 — Router v0 (shell‑first)**  
**Deliverables**

- Shell fast‑path; policy file with allow/deny rules.
- `rlp why "<prompt>"` explains routing.  
  **Exit criteria**
- Interactive “why this route?” shown for three prompts (shell, agent, opening).

**M3.1 — Model broker v0**  
**Deliverables**

- Local+remote providers; per‑request budgets; deterministic mode toggle for
  replay.  
  **Exit criteria**
- Budget/latency visible per request; deterministic replay mode usable in tests.

**M3.2 — Canonical agents & opening**  
**Deliverables**

- `contact_resolver`, `context_gatherer`, `writer`, `critic`, `mailer` (send
  requires human confirm).
- `compose_email` opening (DAG + budgets + success criteria).  
  **Exit criteria**
- End‑to‑end “draft email to John” with confirm before send.

---

### Phase 3 — Openings engine & SDK (M4)

**M4.0 — Opening engine v1**  
**Deliverables**

- Declarative DAG (YAML/DSL): retries, timeouts, budgets, success predicates.
- Deterministic replay over captured traces.  
  **Exit criteria**
- Replay produces identical artifacts/messages for fixed seeds.

**M4.1 — Agent SDK (Rust, wasm32‑wasi)**  
**Deliverables**

- `handle(Message) -> Result<Message)` traits, cap helpers, test harness.  
  **Exit criteria**
- Third‑party example agent compiles to Wasm and runs under `runloopd`.

---

### Phase 4 — Observability & performance harness (M5–M6)

**M5.0 — Observability v1**  
**Deliverables**

- `agtop` pane: per‑agent CPU/RSS, tokens in/out, error rate, cache hits.
- Tracing ladder diagrams in `rlp trace`.
- Per‑opening/agent/provider **cost accounting**.  
  **Exit criteria**
- Kill/restore tests present; trace spans visible for crossings.

**M6.0 — Performance harness & budgets**  
**Deliverables**

- Lab capturing cold/warm startup, message latency, RSS, throughput; dashboards
  against budgets.  
  **Exit criteria**
- Measured: cold start p50 ≤ **40 ms**, agent RSS p50 ≤ **8 MB**, bus throughput
  ≥ **1000 msgs/s** (hardware profile documented).

---

### Phase 5 — Safety, scheduling, and self‑improvement (M8–M9)

**M8.0 — Scheduler & pressure controls**  
**Deliverables**

- Fair‑share per opening; cgroups for CPU/mem/io; soft throttles; quarantine on
  sandbox crash; circuit breakers for flapping agents.  
  **Exit criteria**
- Stress test maintains interactivity while isolating “bad” agents.

**M8.1 — Safety 0.1**  
**Deliverables**

- Capability tokens with expirations; human confirmation for external side
  effects (send/delete/spend).
- Tripwires for outbound spikes and exfil heuristics.  
  **Exit criteria**
- Disallowed actions blocked with human‑readable reasons.

**M9.0 — Self‑improvement harness 0.1**  
**Deliverables**

- Trace capture → clustering → patch proposals (prompts/policies) → sandbox A/B
  → adoption rules; golden task suite + regressions dashboard.  
  **Exit criteria**
- System proposes & validates ≥1 improvement without manual prompt engineering.

---

### Phase 6 — Beta hardening (M10)

**M10.0 — Public Beta (0.9)**  
**Deliverables**

- Installers for Debian/Ubuntu (`.deb`), code‑signed artifacts.
- Docs: Quickstart, SDK, Opening cookbook, Security whitepaper.
- Telemetry **opt‑in** (anonymous) for stability metrics.
- Reliability/performance dashboard tracking acceptance metrics.
- Performance harness methodology captured in [docs/perf.md](docs/perf.md).  
  **Exit criteria (beta gate)**
- 24‑hour soak with **zero critical crashes**; 3 reference openings reproduce
  cleanly; upgrade/downgrade works.

---

### Phase 7 — Release candidate & 1.0 (M11–M12)

**M11.0 — RC (0.99)**  
**Deliverables**

- Backward‑compatible **RMP**/**DSL** finalized; migration scripts.
- API freeze; fault‑injection integration tests.  
  **Exit criteria**
- No known P0/P1 defects; perf & memory targets met.

**M12.0 — v1.0 GA**  
**Deliverables**

- Stable 1.0 release notes, long‑term support & deprecation policy.  
  **Exit criteria**
- All acceptance metrics green; docs complete; supply‑chain attestation
  published.

---

## 4) Ecosystem, packaging & portability (runs alongside Phases 4–7)

### Plugin packaging (bundles)

- Layout:

```text
/agent/
manifest.toml       # name, version, entrypoint, schemas, caps
policy.caps
tools.json          # external tool contracts (see docs/tool-attachments.md)
agent.wasm
LICENSE
README.md

```

- `rlp agent install <path|uri>` validates signature/caps and registers the
  bundle; upgrade path handles versioning.
- Signed bundles (Ed25519), deterministic Wasm builds; trust roots in config.

#### Exit criteria (Bundles)

- Tampered bundle rejected; repro builds verified on two machines;
  install/upgrade rollback tested.

### OS packaging / images

- System user `runloop`; state in `/var/lib/runloop`; per‑user config in
  `~/.runloop`.
- Packages for Debian; demo ISO for fast trial; container image for dev only.

#### Exit criteria (OS packaging)

- `apt install runloop` brings up `runloopd.service` hardened; ISO boots to
  working TUI with sample openings.

### Portability

- Abstract platform shims; Redox PoC (3 canonical agents) to inform future port.

---

## 5) Acceptance metrics & performance budgets

| Metric                 |                  Target (p50 unless noted) | Measured at      |
| ---------------------- | -----------------------------------------: | ---------------- |
| Agent cold start       |                                ≤ **40 ms** | perf lab harness |
| Agent RSS              |                                 ≤ **8 MB** | perf lab harness |
| Bus throughput         |                          ≥ **1000 msgs/s** | perf lab harness |
| Replay determinism     | ≥ **99%** identical outputs on fixed seeds | trace replayer   |
| Crash‑free stability   |       **24 h** soak, zero critical crashes | Beta gate        |
| Human‑confirm coverage |                  100% on send/delete/spend | safety harness   |

> **Notes:** hardware profile, OS, and model/provider mix recorded with every
> run; thresholds are enforced in CI perf jobs.

---

## 6) Interfaces & invariants (normative notes)

- **RMP v0 header (M0.2):** 64-byte, big-endian header with magic `"RMP0"`,
  `header_version=0`, `header_len=64`, zeroed flags/reserved words, `schema_id`
  primitive selector, `body_len`, `created_at_ms`, `ttl_ms`, `trace_id`,
  `msg_id`. TTL enforcement computes `created_at_ms + ttl_ms` in u128 and
  rejects zero/overflow before delivery.
- **KB storage:** canonicalize payload/provenance to **JCS JSON**, hash as
  **BLAKE3 BLOB(32)**; hex form is for logs/UI only.
- **Action confirmation ACL:** only UI may publish `action.decision`; agents
  request via `action.request`.
- **Routing policy:** shell fast‑path togglable; “why this route?” always
  producible.

---

## 7) Docs, ADRs, governance (Phase G, parallel)

- **ADRs:** 0001 Debian + WASM/WASI + SQLite; 0002 RMP v0; 0003 KB event
  sourcing; 0004 Capabilities/security model.
- **Docs:** architecture (C4), protocol, openings DSL, KB schemas, TUI.
- **Repo hygiene:** CONTRIBUTING, CODE_OF_CONDUCT, SECURITY, CODEOWNERS; branch
  protections; labels; issue templates.

### Exit criteria (Docs & Governance)

- ADRs exist & linked; docs lint clean; contributors can build an agent in < 1
  hour using the docs.

---

## 8) Risks & mitigations (live)

- **LLM provider variance** → deterministic broker mode for tests; hybrid
  local/remote with budgets.
- **Perf regressions** → perf gates in CI; nightly dashboards; rolling baseline.
- **Security drift** → capability audit logs; policy tests; signed bundles
  required.
- **Protocol churn** → header versioning; message registry; migration scripts.

---

## 9) How to read this document

- **Deliverables** are artifacts we will ship.
- **Exit criteria** are binary gates that must turn green before the milestone
  closes.
- **Acceptance metrics** are enforced in CI perf jobs and in Beta soak gates.
