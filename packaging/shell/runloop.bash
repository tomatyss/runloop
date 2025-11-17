# runloop router integration for bash

case $- in
  *i*) ;;
  *) return ;;
esac

if [[ -n ${RUNLOOP_ROUTER_BASH_INIT:-} ]]; then
  return
fi

if ! command -v rlp >/dev/null 2>&1; then
  return
fi

RUNLOOP_ROUTER_BASH_INIT=1
: "${RUNLOOP_ROUTER_OPENING_PATH_DEFAULT:=$HOME/.runloop/openings/router-default.yaml}"

runloop_router_off() {
  export RUNLOOP_ROUTER_DISABLE=1
  printf 'runloop router disabled\n'
}

runloop_router_on() {
  unset RUNLOOP_ROUTER_DISABLE
  printf 'runloop router enabled\n'
}

__runloop_router_should_handle() {
  [[ -n ${RUNLOOP_ROUTER_DISABLE:-} ]] && return 1
  [[ $TERM == dumb ]] && return 1
  return 0
}

__runloop_router_opening_path() {
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

__runloop_router_prompt_json() {
  local buffer="$1"
  local escaped=${buffer//\\/\\\\}
  escaped=${escaped//\"/\\\"}
  escaped=${escaped//$'\n'/\\n}
  escaped=${escaped//$'\r'/\\r}
  escaped=${escaped//$'\t'/\\t}
  printf '{"prompt":"%s"}' "$escaped"
}

__runloop_router_invoke() {
  local buffer="$1"
  local opening
  opening=$(__runloop_router_opening_path) || {
    printf 'set RUNLOOP_ROUTER_OPENING_PATH to a valid opening YAML\n' >&2
    return 1
  }
  local params
  params=$(__runloop_router_prompt_json "$buffer") || return 1
  printf '\n'
  RUNLOOP_ROUTER_PROMPT="$buffer" rlp run "$opening" --params "$params"
  local status=$?
  printf '\n'
  return $status
}

__runloop_router_accept_line() {
  local line=$READLINE_LINE
  if [[ -z $line ]] || ! __runloop_router_should_handle; then
    __runloop_router_execute_line "$line"
    return $?
  fi
  local route_output
  route_output=$(printf '%s' "$line" | rlp route --stdin 2>&1)
  local route_status=$?
  if (( route_status == 10 )); then
    __runloop_router_execute_line "$line"
    return $?
  elif (( route_status == 11 )); then
    __runloop_router_run_opening "$line"
    return $?
  fi
  printf 'runloop: router error (%d): %s\n' "$route_status" "$route_output" >&2
  __runloop_router_execute_line "$line"
  return $?
}

__runloop_router_execute_line() {
  local line="$1"
  READLINE_LINE=$line
  READLINE_POINT=${#READLINE_LINE}
  builtin printf '\n'
  builtin eval -- "$line"
  local status=$?
  if __runloop_router_should_record_history "$line"; then
    builtin history -s -- "$line"
  fi
  READLINE_LINE=""
  READLINE_POINT=0
  return $status
}

__runloop_router_run_opening() {
  local line="$1"
  __runloop_router_invoke "$line"
  local status=$?
  if (( status != 0 )); then
    printf 'runloop: failed to execute opening\n' >&2
  fi
  if __runloop_router_should_record_history "$line"; then
    builtin history -s -- "$line"
  fi
  READLINE_LINE=""
  READLINE_POINT=0
  return $status
}

__runloop_router_should_record_history() {
  local line="$1"
  local histcontrol=${HISTCONTROL,,}
  if [[ $histcontrol == *ignorespace* || $histcontrol == *ignoreboth* ]]; then
    [[ $line == ' '* ]] && return 1
  fi
  if [[ $histcontrol == *ignoredups* || $histcontrol == *ignoreboth* ]]; then
    local last
    last=$(builtin history 1 2>/dev/null || printf '')
    last=${last#*  }
    [[ $last == "$line" ]] && return 1
  fi
  if [[ -n ${HISTIGNORE:-} ]]; then
    local IFS=':'
    for pattern in $HISTIGNORE; do
      [[ $line == $pattern ]] && return 1
    done
  fi
  return 0
}

if bind -V >/dev/null 2>&1; then
  bind -x '"\C-m":__runloop_router_accept_line'
  bind -x '"\C-j":__runloop_router_accept_line'
fi
