# Exec hostcall timeouts

The runtime can optionally impose a wall-clock timeout on `exec_spawn`
hostcalls. By default no timeout is applied; commands run until completion. To
enforce an upper bound, set the environment variable `RUNLOOP_EXEC_TIMEOUT_SECS`
to a positive integer number of seconds.

Special values:

- `RUNLOOP_EXEC_TIMEOUT_SECS=0` (or unset) disables any timeout explicitly.

Notes:

- The timeout applies per `exec_spawn` invocation. When exceeded, the child is
  sent a `kill` signal and the hostcall returns `EXEC_ESIGNAL`.
- This setting is process-wide for the daemon; there is currently no per-call or
  per-capability override.
