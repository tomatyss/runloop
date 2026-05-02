# Codex Development Workflow

This guide turns current Codex/AGENTS.md best practices into a Runloop-specific
workflow. It is the detailed companion to the short root
[AGENTS.md](../AGENTS.md).

## Research Basis

Use these sources when updating this workflow:

- OpenAI Codex best practices:
  <https://developers.openai.com/codex/learn/best-practices>
- OpenAI AGENTS.md guidance:
  <https://developers.openai.com/codex/guides/agents-md>
- AGENTS.md open format: <https://agents.md/>
- OpenAI execution-plan cookbook:
  <https://developers.openai.com/cookbook/articles/codex_exec_plans>
- OpenAI note on keeping repo knowledge concise:
  <https://openai.com/index/harness-engineering/>

The practical takeaways are:

- Keep `AGENTS.md` short, accurate, and actionable.
- Put stable, detailed standards in docs and link to them.
- Give Codex clear goal, context, constraints, and done criteria.
- Use plans for complex or multi-hour work.
- Verify with the same checks humans use.

## Instruction Layout

Runloop uses these layers:

1. `AGENTS.md` is the always-loaded project map and rule summary.
2. `CONTINUITY.md` is the compaction-safe ledger for the current workspace.
3. `docs/engineering-standards.md` is the authoritative engineering standard.
4. This file describes the Codex workflow.
5. Future nested `AGENTS.md` files may be added only when a subtree needs
   genuinely different rules.

Avoid adding long examples or duplicated standards to `AGENTS.md`. If the file
starts growing, move detail here or into `docs/engineering-standards.md`.

## Prompt Shape

Good Runloop tasks should include:

- Goal: the exact behavior or artifact wanted.
- Context: relevant crates, files, issues, errors, or commands.
- Constraints: compatibility, security, API, architecture, or rollout limits.
- Done when: tests, CLI behavior, docs, or review outcome that proves success.

Example:

```text
Fix daemon run cancellation.
Context: crates/core/src/control.rs, crates/runloopd/src/control.rs,
crates/runloopd/src/engine.rs, crates/rlp/src/main.rs.
Constraints: preserve existing RunSubmit behavior, no broad refactor.
Done when: cancellation emits a response, active run is stopped or explicitly
reported unsupported, and targeted tests plus cargo test -p runloopd pass.
```

## Default Codex Loop

1. Read and update `CONTINUITY.md`.
2. Inspect `git status --short --branch`.
3. Map the relevant code with `rg`, `rg --files`, and nearby tests.
4. Form a short plan for non-trivial work.
5. Edit only the files required for the task.
6. Run targeted checks first, then broader checks when risk warrants.
7. Review the diff before final response.
8. Update `CONTINUITY.md` with final state and test results.

## Execution Plans

Use an execution plan for work that is ambiguous, architectural, security
sensitive, or likely to take multiple milestones. Store plans under
`docs/exec-plans/` using a dated, descriptive filename, for example:

```text
docs/exec-plans/2026-05-02-run-cancellation.md
```

An execution plan should contain:

- Goal and user-visible success criteria.
- Current behavior and relevant files.
- Proposed design and tradeoffs.
- Step-by-step implementation milestones.
- Test plan.
- Open questions and decisions.
- Progress log.

Keep plans living: update progress after each completed milestone and before any
handoff.

## Verification Matrix

Use the smallest useful check while iterating, then escalate based on risk:

- Formatting/docs only: run the docs formatter; run `cargo fmt --all -- --check`
  if Rust was touched.
- Single Rust crate: run `cargo test -p <crate>`. Escalate to
  `cargo clippy --workspace -- -D warnings` before broader handoff.
- Shared crates (`core`, `rmp`): run known dependent crate tests, then
  `cargo test --workspace` when behavior may cross crate boundaries.
- Runtime/caps/secrets/hostcalls: run `cargo test -p runloop-runtime`, then
  `cargo test --workspace` plus a security-focused review.
- Daemon/control/bus: run `cargo test -p runloopd` and relevant bus tests.
  Escalate to `cargo test --workspace` for behavior changes.
- Opening parser/runner: run `cargo test -p runloop-openings`. Add
  executor-local integration tests for execution behavior.
- CLI agent install/scaffold: run targeted `cargo test -p rlp` filters. Use the
  package smoke path when packaging behavior changes.
- WASM agents: run `just build-agents-wasm`; run `just test-agents-wasm` before
  handoff when agent behavior changes.

Always report ignored tests when they are relevant. Current normal workspace
test run ignores:

- `golden_compose_email`
- `system_tra_opening_runs_with_structured_input`

## Review Workflow

For review requests:

- Start with actionable findings, ordered by severity.
- Include tight file/line references.
- Focus on correctness, security, regressions, and missing tests.
- Keep summaries secondary.
- If no issues are found, state that clearly and list residual risk or test
  gaps.

For security-sensitive code, check:

- capability enforcement before host effects
- path traversal and symlink handling
- secret exposure and logging
- spoofable bus/control messages
- idempotency and replay behavior
- audit records on allow and deny paths

## Codex Configuration Suggestions

Repository settings should be conservative. Personal preferences belong in
`~/.codex/config.toml`; repo behavior belongs in `.codex/config.toml` only when
the team agrees it should be shared.

Recommended personal defaults for this repo:

```toml
model_reasoning_effort = "medium"
plan_mode_reasoning_effort = "high"
approval_policy = "on-request"
sandbox_mode = "workspace-write"
project_doc_max_bytes = 32768
```

Do not check in secrets, credentials, local paths, or personal model choices.

## Maintenance

Update this workflow when:

- a repeated Codex failure has a durable fix
- CI commands change
- a crate gains special setup or test requirements
- new security-sensitive surfaces are introduced

Do not add speculative rules. Rules should reflect real project behavior.
