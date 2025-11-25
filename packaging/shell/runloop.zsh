# runloop router integration for zsh
# Optional knobs: RUNLOOP_ROUTER_BINDKEY='^J', RUNLOOP_ROUTER_TIMEOUT_MS=200,
# RUNLOOP_ROUTER_DISABLE=1, RUNLOOP_ROUTER_FORCE=1
# shellcheck disable=SC2154

if [[ ! -o interactive ]]; then
  return
fi

if [[ -n ${RUNLOOP_ROUTER_ZSH_INIT:-} ]]; then
  return
fi

if ! command -v rlp >/dev/null 2>&1; then
  return
fi

typeset -g RUNLOOP_ROUTER_ZSH_INIT=1

: ${RUNLOOP_ROUTER_OPENING_PATH_DEFAULT:=${HOME}/.runloop/openings/router-default.yaml}

runloop_router_off() {
  export RUNLOOP_ROUTER_DISABLE=1
  if whence zle >/dev/null 2>&1; then
    zle -M "runloop router disabled"
  fi
}

runloop_router_on() {
  unset RUNLOOP_ROUTER_DISABLE
  if whence zle >/dev/null 2>&1; then
    zle -M "runloop router enabled"
  fi
}

_runloop_router_should_handle() {
  [[ -o interactive ]] || return 1
  [[ -n ${RUNLOOP_ROUTER_FORCE:-} ]] && return 0
  [[ -n ${RUNLOOP_ROUTER_DISABLE:-} ]] && return 1
  [[ "$TERM" == "dumb" ]] && return 1
  local auto_envs=(
    CI GITHUB_ACTIONS BUILDKITE TEAMCITY_VERSION JENKINS_URL
    GITLAB_CI CIRCLECI SSH_CONNECTION SSH_TTY
  )
  local name
  for name in $auto_envs; do
    [[ -n ${(P)name} ]] && return 1
  done
  return 0
}

_runloop_router_opening_path() {
  if [[ -n ${RUNLOOP_ROUTER_OPENING_PATH:-} && -f $RUNLOOP_ROUTER_OPENING_PATH ]]; then
    printf '%s' "$RUNLOOP_ROUTER_OPENING_PATH"
    return 0
  fi
  if [[ -f $RUNLOOP_ROUTER_OPENING_PATH_DEFAULT ]]; then
    printf '%s' "$RUNLOOP_ROUTER_OPENING_PATH_DEFAULT"
    return 0
  fi
  return 1
}

_runloop_router_prompt_json() {
  local buffer="$1"
  local escaped=${buffer//\\/\\\\}
  escaped=${escaped//\"/\\\"}
  escaped=${escaped//$'\n'/\\n}
  escaped=${escaped//$'\r'/\\r}
  escaped=${escaped//$'\t'/\\t}
  printf '{"prompt":"%s"}' "$escaped"
}

_runloop_router_invoke() {
  local buffer="$1"
  local opening
  opening=$(_runloop_router_opening_path) || {
    zle -M "set RUNLOOP_ROUTER_OPENING_PATH to a valid opening YAML"
    return 1
  }
  local params
  params=$(_runloop_router_prompt_json "$buffer") || return 1
  printf '\n'
  RUNLOOP_ROUTER_PROMPT="$buffer" rlp run "$opening" --params "$params"
  local status=$?
  printf '\n'
  return $status
}

runloop_accept_line() {
  emulate -L zsh
  setopt localoptions no_beep noshwordsplit pipe_fail
  local buffer="$BUFFER"
  if [[ -z $buffer ]] || ! _runloop_router_should_handle; then
    zle .accept-line
    return
  fi
  local route_cmd=(rlp route --stdin)
  if [[ -n ${RUNLOOP_ROUTER_TIMEOUT_MS:-} ]]; then
    route_cmd+=(--timeout-ms "$RUNLOOP_ROUTER_TIMEOUT_MS")
  fi
  local route_output
  route_output=$(printf '%s' "$buffer" | "${route_cmd[@]}" 2>&1)
  local route_status=$?
  if (( route_status == 10 )); then
    zle .accept-line
    return
  elif (( route_status == 11 )); then
    if _runloop_router_invoke "$buffer"; then
      BUFFER=""
      CURSOR=0
      zle redisplay
    else
      zle -M "runloop: failed to execute opening; start runloopd or add --local to the opening"
    fi
    return
  elif (( route_status == 12 )); then
    zle -M "runloop: router timeout; executing in shell"
    zle .accept-line
    return
  fi
  zle -M "runloop: router error ($route_status): $route_output"
  zle .accept-line
}

zle -N runloop-accept-line runloop_accept_line
_runloop_bind=${RUNLOOP_ROUTER_BINDKEY:-'^M'}
bindkey "$_runloop_bind" runloop-accept-line &>/dev/null || true
if bindkey -M viins >/dev/null 2>&1; then
  bindkey -M viins "$_runloop_bind" runloop-accept-line
fi
if bindkey -M vicmd >/dev/null 2>&1; then
  bindkey -M vicmd "$_runloop_bind" runloop-accept-line
fi
