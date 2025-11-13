# Architecture Decision Records (ADRs)

ADRs capture irreversible technical choices so future contributors understand
why Runloop’s architecture looks the way it does. Keep every ADR in this
directory and reference them from specs, code comments, and PRs.

## Numbering & Naming

- Use zero-padded identifiers (`0001`, `0002`, …). The file name should match
  the identifier and a short slug, e.g., `0001-debian-wasm-wasi-sqlite.md`.
- Reserve the following IDs per the roadmap:
  - `0001` — Debian base image, WASM/WASI runtime, SQLite storage.
  - `0002` — Runloop Message Protocol (RMP) v0 framing & registry policy.
  - `0003` — Personal Ops Graph (POG) event-sourcing model.
  - `0004` — Capabilities & security enforcement model.
  Create new ADRs starting at `0005`.

## Template

Include the sections below (feel free to copy/paste when creating a new file):

````markdown
---
adr: 0005
title: Short Title
status: Proposed | Accepted | Superseded
date: 2025-11-13
deciders: names/handles
tags: [runtime, kb]
---

## Context
Problem being solved, constraints, prior art.

## Decision
The option we picked and why.

## Consequences
Positive and negative fallout, follow-up tasks.

## Alternatives Considered
Bulleted list with 2–3 sentences on why each was rejected.
````

## Workflow

1. Draft the ADR in a PR (or convert a design issue into a PR once you have a
   preferred direction).
2. Request review from the stakeholders listed in the relevant roadmap phase
   (runtime, KB, policy, etc.).
3. Once approved, merge the ADR and immediately link it from the relevant docs
   (`README.md`, `docs/architecture.md`, protocol specs, etc.).
4. If an ADR becomes obsolete, create a follow-up ADR marked “Supersedes
   <old-id>” and update the old file’s status to `Superseded` with a backlink.

## Storage & Cross-References

- Keep ADRs under version control; do not edit them retroactively after they
  are marked `Accepted` except to fix broken links or typos.
- Reference ADR IDs in code comments (`// ADR-0002: reason`), docs, and commit
  messages to provide traceability.
- When adding new components or policies, check existing ADRs first to avoid
  conflicting decisions. If a conflict is unavoidable, call it out explicitly
  in the new ADR.
