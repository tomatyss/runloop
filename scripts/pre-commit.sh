#!/usr/bin/env bash
# Pre-commit workflow for Runloop OS.
# Runs the same gates required for CI so local commits stay green.

set -euo pipefail

ROOT_DIR="$(git rev-parse --show-toplevel)"
cd "$ROOT_DIR"

log() {
  printf "\e[34m[pre-commit]\e[0m %s\n" "$@"
}

maybe_skip() {
  local name="$1"
  local env_flag="$2"
  local default="$3"

  # Allow toggling individual steps with RUNLOOP_PRECOMMIT_* env vars.
  if [[ "${env_flag}" == "auto" ]]; then
    env_flag="RUNLOOP_PRECOMMIT_${name}"
  fi

  local value="${!env_flag:-$default}"
  [[ "$value" == "0" ]]
}

if maybe_skip "FMT" "RUNLOOP_PRECOMMIT_FMT" "0"; then
  log "Checking formatting (cargo fmt --all -- --check)"
  if ! cargo fmt --all -- --check; then
    log "Formatting issues detected; run 'cargo fmt --all' and restage changes."
    exit 1
  fi
else
  log "Skipping formatting (RUNLOOP_PRECOMMIT_FMT=${RUNLOOP_PRECOMMIT_FMT:-1})"
fi

if maybe_skip "CLIPPY" "RUNLOOP_PRECOMMIT_CLIPPY" "0"; then
  log "Running cargo clippy --workspace -- -D warnings"
  cargo clippy --workspace -- -D warnings
else
  log "Skipping clippy (RUNLOOP_PRECOMMIT_CLIPPY=${RUNLOOP_PRECOMMIT_CLIPPY:-1})"
fi

if maybe_skip "MARKDOWN" "RUNLOOP_PRECOMMIT_MARKDOWN" "0"; then
  if command -v npx >/dev/null 2>&1; then
    log "Linting markdown (npx markdownlint-cli2 \"docs/**/*.md\")"
    npx markdownlint-cli2 "docs/**/*.md"
  else
    log "Skipping markdownlint (npx not found; install Node.js/npm or export RUNLOOP_PRECOMMIT_MARKDOWN=1)"
  fi
else
  log "Skipping markdownlint (RUNLOOP_PRECOMMIT_MARKDOWN=${RUNLOOP_PRECOMMIT_MARKDOWN:-1})"
fi

if maybe_skip "TESTS" "RUNLOOP_PRECOMMIT_TESTS" "0"; then
  log "Running cargo test --workspace"
  cargo test --workspace
else
  log "Skipping tests (RUNLOOP_PRECOMMIT_TESTS=${RUNLOOP_PRECOMMIT_TESTS:-1})"
fi
