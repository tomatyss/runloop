# Prompt Routing & Shell Integration

> **Doc status:** MVP implementation guide for Epic O. Last updated: 2025-11-25.

This guide explains how Runloop classifies interactive prompts (`rlp route`),
why/when they are sent to agents, and how to wire the provided zsh/bash snippets
so that your terminal consults the router before executing a command.

## 1. Router quick reference

- `rlp route "<prompt>"` (or `rlp route --stdin`) prints a stable JSON payload
  `{ "version": 1, "route": "shell|agent", "rule": "...", "blocked": bool }`. It
  exits `10` for shell decisions, `11` for agent decisions, and `12` on a
  timeout (`--timeout-ms` or `RUNLOOP_ROUTER_TIMEOUT_MS`, default 200 ms) so
  shells can branch on `$?` without parsing stdout.
- `rlp why "<prompt>" --json|--table` shows the matched rule, allow/deny hits,
  and any features (POSIX tokens, known commands, heuristics).
- Configuration lives under `router.*` in `config.yaml`:
  - `router.fastpath_shell`: disable to always route to the default opening.
  - `router.default_opening`: semantic name used in traces/telemetry.
  - `router.allowlist` / `router.denylist`: substring matches that force shell
    routing or block prompts entirely.
  - `router.known_commands`: extends the builtin command dictionary feeding the
    heuristics.
- All routing is local: there is no KB/model work in `rlp route`, so it is safe
  to call on every prompt.

## 2. Shell helper (`rlp shell`)

The CLI manages shell snippets via `rlp shell`:

```bash
# inspect without changing files
rlp shell enable --shell zsh --dry-run --snippet packaging/shell/runloop.zsh \
  --opening "$HOME/.runloop/openings/router-default.yaml"

# write the block into ~/.zshrc (default rc path)
rlp shell enable --shell zsh --snippet packaging/shell/runloop.zsh \
  --opening "$HOME/.runloop/openings/router-default.yaml"

# remove it later
rlp shell disable --shell zsh
```

Key behaviors:

- The helper looks for snippets under `/usr/share/runloop/shell` (packaged
  installs), `/usr/local/share/runloop/shell`, `~/.runloop/shell`, and—as a
  convenience when working inside the repo—`packaging/shell/`.
- `--snippet <path>` overrides discovery; the command errors if the file does
  not exist (use `--dry-run` to preview). The helper canonicalizes snippet and
  opening paths before writing, so relative inputs like
  `--snippet packaging/shell/runloop.zsh` remain valid once you start new shell
  sessions elsewhere (the block stores an absolute path).
- `--opening <path>` injects an `export RUNLOOP_ROUTER_OPENING_PATH=...` line so
  the snippet knows which YAML opening to use when routing to agents. If you
  omit `--opening`, the helper copies
  `/usr/share/runloop/openings/router-default.yaml` (identical to
  `examples/openings/compose_email.yaml`) into
  `~/.runloop/openings/router-default.yaml` **if that file is absent**, and
  points the env var at the user copy. Existing user copies are never
  overwritten; if neither path exists you can still pass `--opening` later.

- All edits are wrapped by a marker block, e.g.

  ```text
  # >>> runloop shell (zsh) >>>
  export RUNLOOP_ROUTER_OPENING_PATH='/home/me/.runloop/openings/router-default.yaml'
  source '/usr/share/runloop/shell/runloop.zsh'
  # <<< runloop shell (zsh) <<<
  ```

- `rlp shell disable` removes the block (idempotent) and keeps timestamped
  backups (`.zshrc.runloop.bak.<epoch>`).

## 3. Zsh integration (`packaging/shell/runloop.zsh`)

The zsh snippet installs a ZLE widget `runloop-accept-line` that wraps the
builtin `accept-line` and behaves as follows:

1. Guardrails: only runs in interactive shells, skips when `$TERM=dumb`, when
   `RUNLOOP_ROUTER_DISABLE=1`, when common CI/SSH envs are present
   (`CI|GITHUB_ACTIONS|BUILDKITE|TEAMCITY_VERSION|JENKINS_URL|GITLAB_CI|CIRCLECI|SSH_CONNECTION|SSH_TTY`),
   or when `rlp` is missing from `$PATH`. Override with `RUNLOOP_ROUTER_FORCE=1`
   or toggle mid-session via `runloop_router_on/off`.
2. On Enter it calls `rlp route --stdin`, ignoring stdout and branching on the
   exit code:
   - `10` → shell fast-path: call `zle .accept-line` so the command executes
     normally.
   - `11` → agent path: convert the buffer into JSON (`{"prompt":"..."}`), call
     `rlp run <opening.yaml> --params '<json>'`, clear the buffer, and redraw
     the prompt. The default opening path resolves from
     `RUNLOOP_ROUTER_OPENING_PATH` (injected by the helper) or the fallback
     `~/.runloop/openings/router-default.yaml` if it exists.
   - `12` → router timeout: shows `runloop: router timeout; executing in shell`
     and falls back to the shell.
   - other exit codes → warn via `zle -M` and fall back to the shell.
3. Helper functions:
   - `runloop_router_off` / `runloop_router_on` export or unset
     `RUNLOOP_ROUTER_DISABLE` mid-session.
   - `_runloop_router_prompt_json` handles basic escaping (quotes, backslashes,
     newlines) to avoid external dependencies.
4. Key binding: defaults to `^M` (Enter). Override with
   `RUNLOOP_ROUTER_BINDKEY='^J'` (set before sourcing) to avoid conflicts with
   oh-my-zsh/Prezto keymaps; the widget binds in the main, `viins`, and `vicmd`
   maps.

### Requirements & tips

- Ensure `rlp run` can reach `runloopd` (or pass `--local` via your opening) so
  agent runs succeed; failures are echoed inline and the original command is not
  executed.
- When copying additional openings for shell routing, keep them under a writable
  directory (`~/.runloop/openings/` or `/usr/share/runloop/openings/`).
- Use `RUNLOOP_ROUTER_DISABLE=1 zsh` to start a shell with routing disabled; run
  `runloop_router_on` later to re-enable.

## 4. Bash integration (`packaging/shell/runloop.bash`)

The bash snippet wires a `bind -x` handler for `Ctrl-M` / `Ctrl-J` (Enter). The
handler inspects `$READLINE_LINE`, calls `rlp route`, and either evaluates the
line via `eval` (shell route) or invokes the opening (agent route). Behavior
mirrors the zsh widget with bash-specific nuances:

- Execution records history using the same `HISTCONTROL` / `HISTIGNORE`
  semantics as readline (leading-space secrets and ignored patterns remain out
  of history).
- Agent runs print a newline, run `rlp run <opening.yaml> --params ...`, and
  return to the prompt without executing the buffer.
- Exit codes propagate: `$?`, `&&`/`||`, and `set -e` behave as if the user
  executed the command or opening manually.
- `runloop_router_off` / `runloop_router_on` work just like the zsh helpers.
- Because bash lacks a native status bar, errors are printed to stderr.
- The snippet avoids non-interactive shells by examining `$-`; sourcing it in
  scripts is a no-op.

Limitations (MVP):

- Multi-line PS2 prompts are not currently intercepted; the router only handles
  single-line READLINE buffers.
- If `rlp route` exits non-zero (e.g., misconfigured config), the line executes
  as a shell command after printing the error message.

## 5. Runtime toggles & troubleshooting

<!-- markdownlint-disable MD013 -->

| Toggle / Command                      | Effect                                                                          |
| ------------------------------------- | ------------------------------------------------------------------------------- |
| `RUNLOOP_ROUTER_DISABLE=1`            | Globally disable routing (shells fall back to normal behavior).                 |
| `RUNLOOP_ROUTER_FORCE=1`              | Override CI/SSH auto-disable and force routing on.                              |
| `runloop_router_off/on`               | Convenience helpers provided by both snippets.                                  |
| `RUNLOOP_ROUTER_OPENING_PATH`         | Absolute path to the YAML opening to invoke for agent routes.                   |
| `RUNLOOP_ROUTER_OPENING_PATH_DEFAULT` | Optional fallback path (defaults to `~/.runloop/openings/router-default.yaml`). |
| `RUNLOOP_ROUTER_TIMEOUT_MS`           | Timeout for `rlp route` (default 200); exit code 12 on expiry.                  |
| `RUNLOOP_ROUTER_BINDKEY`              | Key sequence to bind the widget (default `^M`).                                 |
| `RUNLOOP_ROUTER_DEBUG=1` _(future)_   | Reserved for verbose logging (not yet implemented).                             |

<!-- markdownlint-enable MD013 -->

Common issues:

- **`snippet not found`** – pass `--snippet <path>` to `rlp shell enable` or
  copy the repo snippets to `~/.runloop/shell/`.
- **`set RUNLOOP_ROUTER_OPENING_PATH` warning** – ensure the referenced YAML
  exists; the helper does not create it for you.
- **Daemon unreachable** – `rlp run` will print `daemon ... unreachable`; start
  `runloopd` or rerun with `rlp run --local` inside your opening.
- **Inspecting decisions** – run `rlp why "<prompt>"` after the fact to see the
  rule/feature breakdown.
- **Rolling back** – run `rlp shell disable --shell <zsh|bash>` to remove the
  block or manually delete the marker section.

## 6. Linking from other docs

- `README.md` references this document in the CLI/TUI section.
- `docs/architecture.md` replaces the “forthcoming router section” with a link
  here.
- `docs/SUMMARY.md` lists it under “Tools” for mdBook navigation.

With the helper and snippets in place, interactive prompts in supported shells
are classified before execution, shell-routed commands behave normally, and
agent-routed prompts flow through `rlp run` without manual wiring.
