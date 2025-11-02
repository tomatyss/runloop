## Phase 0 — Create the repository shell

1. **Create a new repo**

* [x] Repo name: `runloop`
* [x] Default branch: `main`
* [x] Visibility: your choice (private while incubating; public if ready)
* [x] Initialize with **no** sample code

2. **Top‑level scaffolding (empty files are okay)**

* [x] `README.md`
* [x] `LICENSE` (choose: Apache‑2.0 recommended)
* [x] `CODE_OF_CONDUCT.md` (Contributor Covenant v2.1 template)
* [x] `CONTRIBUTING.md`
* [x] `SECURITY.md` (how to report vulnerabilities)
* [x] `SUPPORT.md` (where to get help)
* [x] `CODEOWNERS` (set ownership by path)
* [x] `.gitignore` (Rust, Cargo, target/, build artifacts, OS junk)
* [x] `.gitattributes` (text=auto, eol=lf; mark binaries)
* [x] `.editorconfig` (tabs/spaces, utf‑8, newline rules)
* [x] `rust-toolchain.toml` (pin to `stable`, add `clippy`, `rustfmt`)
* [x] `CHANGELOG.md` (empty; explain Conventional Commits)
* [x] `Justfile` or `Makefile` (task aliases; can be empty/commented)
* [x] `.env.example` (document expected env vars; **no secrets**)

**Definition of Done (DoD):** Repo contains these files with initial headings and TODOs, committed to `main`.

---

## Phase 1 — Document the project intent & layout

3. **README.md — initial content outline**

* [ ] One‑sentence elevator pitch (agent‑native, terminal‑first OS)
* [ ] “Why Runloop?” (problem & philosophy)
* [ ] High‑level architecture diagram placeholder (link to `docs/architecture.md`)
* [ ] Quickstart table of contents pointing to docs (no commands yet)
* [ ] Project status (alpha, roadmap link)
* [ ] Contributing & community links
* [ ] License & security disclosure

4. **/docs tree (no implementation)**
   Create the following placeholders with headings and bullets (no code required):

* [x] `docs/architecture.md`
  Sections: Goals, Non‑goals, Components (router, openings engine, runtime, KB, model broker, TUI), Data flow (messages, events), Trust & capabilities model, Portability plan (Debian→Redox).
* [x] `docs/roadmap.md`
  Sections: Phases A–F, milestones, exit criteria (link to your earlier roadmap).
* [x] `docs/getting-started.md`
  Sections: prerequisites, repo layout, how to read the repo, where to find tasks.
* [x] `docs/contributor-guide.md`
  Sections: how to pick an issue, style conventions, review process.
* [x] `docs/release-process.md`
  Sections: versioning & tags, CHANGELOG rules, release approvals, artifacts (deb, ISO).
* [x] `docs/security-model.md`
  Sections: sandbox/capabilities, secret handling, provenance & audit, threat model (to fill later).
* [x] `docs/message-protocol.md`
  Sections: header fields (trace_id etc), content types, schemas registry, delivery guarantees (conceptual).
* [x] `docs/kb-schemas.md`
  Sections: events (identity, contact, artifact…), materialized views, provenance rules, retention.
* [x] `docs/openings-dsl.md`
  Sections: basic grammar, nodes/edges/policy, replay semantics, examples (descriptive, no code).
* [x] `docs/tui.md`
  Sections: panes, keybinds, UX principles, accessibility.
* [x] `docs/ops.md`
  Sections: packaging, systemd units, live‑build, CI artifacts, SBOM plan.

**DoD:** All documents exist with section headings and TODO bullets; README links are not broken.

---

## Phase 2 — Define repository structure (directories only)

5. **Top‑level layout (create empty dirs & placeholder READMEs)**

* [x] `/crates/` (Rust workspace crates — **no code** yet)

  * [x] `/crates/runloopd/` (daemon) — add `README.md` (scope & interfaces to be implemented)
  * [x] `/crates/rlp/` (CLI)
  * [x] `/crates/agtop/` (monitor TUI)
  * [x] `/crates/runtime/` (Wasm runtime, caps)
  * [x] `/crates/rmp/` (Runloop Message Protocol types/codec)
  * [x] `/crates/kb/` (knowledge base)
  * [x] `/crates/model-broker/` (LLM provider abstraction)
  * [x] `/crates/sdk/` (agent SDK)
* [x] `/agents/` (agent bundles)

  * [x] `/agents/contact_resolver/` (placeholder only)
  * [x] `/agents/context_gatherer/`
  * [x] `/agents/writer/`
  * [x] `/agents/critic/`
  * [x] `/agents/mailer/`
* [x] `/examples/`

  * [x] `/examples/openings/` (YAML samples; put `.placeholder` files)
  * [x] `/examples/config/` (`config.yaml` placeholder)
* [x] `/packaging/`

  * [x] `/packaging/systemd/` (`runloopd.service` placeholder, no unit spec yet)
  * [x] `/packaging/live-build/` (folders only; see Phase 4)
  * [x] `/packaging/container/` (Dockerfile.dev placeholder, no commands)
* [x] `/docs/` (from Phase 1)
* [x] `/infra/`

  * [x] `/infra/ci/` (workflows live in `.github/workflows`, but keep notes here)
  * [x] `/infra/release/` (release notes templates, SBOM plan)
* [x] `/.github/`

  * [x] `/.github/ISSUE_TEMPLATE/bug_report.md` (template—headings only)
  * [x] `/.github/ISSUE_TEMPLATE/feature_request.md`
  * [x] `/.github/ISSUE_TEMPLATE/task.md`
  * [x] `/.github/pull_request_template.md`
  * [x] `/.github/workflows/` (empty for now; see Phase 3)

**DoD:** Directory tree exists, each directory has a short `README.md` explaining scope and interfaces to expect.

---

## Phase 3 — Project policy & collaboration hygiene

6. **Contribution policy (CONTRIBUTING.md)**

* [ ] Project scope & expectations
* [ ] How to file issues (types: bug/feature/task)
* [ ] Branching model (e.g., feature branches, PRs to `main`)
* [ ] Coding style references (point to docs and rustfmt/clippy, but no code)
* [ ] Review & approval (required reviewers, CODEOWNERS)
* [ ] DCO or CLA policy (choose and document)
* [ ] Conventional Commits (types: feat/fix/chore/docs/refactor/test/build/ci)

7. **Code of Conduct (CODE_OF_CONDUCT.md)**

* [ ] Add Contributor Covenant headings
* [ ] Maintainer contact for incidents

8. **Security policy (SECURITY.md)**

* [ ] Disclosure email or process
* [ ] Target response windows
* [ ] Supported branches policy

9. **CODEOWNERS**

* [ ] Assign owners by path (`/crates/*`, `/docs/*`, `/packaging/*`, etc.)
* [ ] Fallback owner for root

10. **Repository settings (admin UI tasks)**

* [ ] Protect `main` (require PR, 1+ reviews, linear history, no force push)
* [ ] Require status checks (to be added in Phase 5)
* [ ] Enable secret scanning, Dependabot alerts (if using GitHub)
* [ ] Default labels: `bug`, `feature`, `task`, `infra`, `docs`, `security`, `good-first-issue`

**DoD:** Policies are merged; branch protections enabled; labels exist.

---

## Phase 4 — Packaging skeletons (no implementation)

11. **Systemd unit placeholders**

* [ ] `packaging/systemd/README.md` (describe units to come: `runloopd.service`, others)
* [ ] `packaging/systemd/runloopd.service.placeholder`
  Content bullets: description, After/Wants, ExecStart path, Restart policy, WantedBy.

12. **Live‑build skeleton (folders only, doc pointers)**

* [ ] `packaging/live-build/auto/` (explain `lb config` parameters in README)
* [ ] `packaging/live-build/config/package-lists/` (note: where packages get listed)
* [ ] `packaging/live-build/config/hooks/normal/` (note: where install hooks run)
* [ ] `packaging/live-build/config/includes.chroot/` (note: where files land inside ISO)
* [ ] `packaging/live-build/README.md`
  Sections: image type (iso‑hybrid), Debian release (bookworm), how our `.deb`s are staged (conceptual only)

13. **Container skeleton**

* [ ] `packaging/container/README.md`
  Sections: dev container purpose; difference from VM/ISO; volumes layout; no Dockerfile yet

**DoD:** Packaging dirs exist with READMEs that explain intent and expected artifacts.

---

## Phase 5 — CI/CD scaffold (workflows names only; no build steps)

14. **Workflow placeholders in `.github/workflows/`**

* [ ] `ci-build.yml`
  Sections: triggers (PR, push to main); goals (build, lint, test); artifact note (future)
* [ ] `ci-security.yml`
  Sections: dependency audit, license scan (concept), code scanning
* [ ] `release.yml`
  Sections: trigger on tag; build artifacts (.deb, ISO); sign & upload (concept)
* [ ] `docs-check.yml`
  Sections: broken links, spellcheck, Markdown lint

15. **CI documentation**

* [ ] `infra/ci/README.md`
  Sections: workflow purposes, required secrets, caching policy (concept), branch policies

**DoD:** Workflows are present as placeholders with clear goals and TODO comments (no jobs yet).

---

## Phase 6 — Configuration, styles, conventions

16. **.editorconfig**

* [ ] UTF‑8
* [ ] LF newlines
* [ ] 2 or 4 spaces (choose and document)
* [ ] Trim trailing whitespace, insert final newline

17. **.gitattributes**

* [ ] `* text=auto eol=lf`
* [ ] `*.png binary` and other binaries flagged
* [ ] Mark large generated artifacts to be treated as binary

18. **Commit conventions**

* [ ] Document Conventional Commits in `CONTRIBUTING.md`
* [ ] Add examples (no code; show message format only)

19. **Versioning & releases (`docs/release-process.md`)**

* [ ] SemVer policy (pre‑1.0 rules)
* [ ] Tagging scheme (`v0.x.y`)
* [ ] CHANGELOG sections (Added/Changed/Fixed/Deprecated/Removed/Security)
* [ ] Release approvals & checklist

**DoD:** Style and release conventions are documented and referenced from README.

---

## Phase 7 — Architecture records & traceability

20. **ADRs (Architecture Decision Records)**

* [ ] `docs/adr/0001-debian-wasm-wasi-sqlite.md`
  Sections: context, decision, consequences, alternatives considered
* [ ] `docs/adr/0002-message-protocol-rmp.md`
* [ ] `docs/adr/0003-kb-event-sourcing.md`
* [ ] `docs/adr/0004-capabilities-security-model.md`
* [ ] `docs/adr/README.md` (how to add ADRs; numbering conventions)

**DoD:** ADRs exist with decision titles and outlines; linked from `docs/architecture.md`.

---

## Phase 8 — Issue taxonomy & initial backlog (no code)

21. **Issue templates (content prompts only)**

* [ ] Bug report: expected/actual, repro steps, logs (concept), environment
* [ ] Feature request: user story, acceptance criteria, out‑of‑scope
* [ ] Task: description, definition of done, dependencies

22. **Seed the first epic issues (links to docs)**

* [ ] “Repo bootstrap” epic linking this checklist
* [ ] “Runtime skeleton” epic (description only)
* [ ] “Protocol docs” epic (docs only)
* [ ] “KB schemas doc” epic
* [ ] “Packaging plan doc” epic
* [ ] “CI design doc” epic

**DoD:** Backlog exists with epics and child tasks tied to docs pages.

---

## Phase 9 — Developer UX notes (no build/run)

23. **docs/local-dev-setup.md**

* [ ] Host OS assumptions (Debian/Ubuntu or dev container)
* [ ] Tooling list (Rust toolchain, `just`, etc.—without install commands)
* [ ] Folder structure tour
* [ ] How to navigate TUI docs, openings DSL docs, protocol docs

24. **Editor setup (`.vscode/` optional)**

* [ ] `.vscode/extensions.json` (suggest rust‑analyzer, markdown lint)
* [ ] `.vscode/settings.json` (format on save, end‑of‑line LF)

**DoD:** New contributors can read and understand how to get oriented without running code.

---

## Phase 10 — Final sanity checklist before inviting contributors

* [ ] `README.md` has a clear “What this repo is” and “What it is not” section
* [ ] All links inside README → docs resolve
* [ ] Every directory has a `README.md` explaining its purpose
* [ ] Policies present: `CONTRIBUTING`, `CODE_OF_CONDUCT`, `SECURITY`, `LICENSE`
* [ ] Governance: `CODEOWNERS` set, branch protections enabled
* [ ] `.github` templates exist; default labels are created
* [ ] ADRs exist; architecture and roadmap are sketched
* [ ] No secrets, tokens, or private data in repo history

**DoD:** A new developer can clone the repo, read the docs, understand scope and structure, and open issues/PRs—**without needing any implementation code.**

---

## Optional extras (still no code)

* [ ] `docs/terminology.md` (Runloop vocabulary: trajectories, crossings, openings)
* [ ] `docs/checklists/` (release checklist, security review checklist)
* [ ] `docs/style-guides/` (doc style, API naming)
* [ ] `GOVERNANCE.md` (decision‑making, maintainers)
* [ ] `FUNDING.yml` or `SPONSORS.md` (if relevant)

---

### Copy‑ready repo tree (placeholders only)

```
runloop/
├─ README.md
├─ LICENSE
├─ CODE_OF_CONDUCT.md
├─ CONTRIBUTING.md
├─ SECURITY.md
├─ SUPPORT.md
├─ CODEOWNERS
├─ .gitignore
├─ .gitattributes
├─ .editorconfig
├─ rust-toolchain.toml
├─ CHANGELOG.md
├─ .env.example
├─ Justfile        # or Makefile (can be empty)
├─ docs/
│  ├─ architecture.md
│  ├─ roadmap.md
│  ├─ getting-started.md
│  ├─ contributor-guide.md
│  ├─ release-process.md
│  ├─ security-model.md
│  ├─ message-protocol.md
│  ├─ kb-schemas.md
│  ├─ openings-dsl.md
│  ├─ tui.md
│  ├─ ops.md
│  └─ adr/
│     ├─ 0001-debian-wasm-wasi-sqlite.md
│     ├─ 0002-message-protocol-rmp.md
│     ├─ 0003-kb-event-sourcing.md
│     ├─ 0004-capabilities-security-model.md
│     └─ README.md
├─ crates/
│  ├─ runloopd/README.md
│  ├─ rlp/README.md
│  ├─ agtop/README.md
│  ├─ runtime/README.md
│  ├─ rmp/README.md
│  ├─ kb/README.md
│  ├─ model-broker/README.md
│  └─ sdk/README.md
├─ agents/
│  ├─ contact_resolver/README.md
│  ├─ context_gatherer/README.md
│  ├─ writer/README.md
│  ├─ critic/README.md
│  └─ mailer/README.md
├─ examples/
│  ├─ openings/README.md
│  └─ config/README.md
├─ packaging/
│  ├─ systemd/
│  │  ├─ runloopd.service.placeholder
│  │  └─ README.md
│  ├─ live-build/
│  │  ├─ auto/        # empty; explained in README
│  │  └─ config/      # empty; explained in README
│  └─ container/
│     └─ README.md
├─ infra/
│  ├─ ci/README.md
│  └─ release/README.md
└─ .github/
   ├─ ISSUE_TEMPLATE/
   │  ├─ bug_report.md
   │  ├─ feature_request.md
   │  └─ task.md
   ├─ pull_request_template.md
   └─ workflows/
      ├─ ci-build.yml
      ├─ ci-security.yml
      ├─ release.yml
      └─ docs-check.yml
```
