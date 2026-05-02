# Capability Policy Format (Draft)

> **Doc status:** Draft — schema and override semantics are normative for v0.1.
> Last updated: 2025-11-02.

Agents declare their required capabilities in `policy.caps` files bundled with
the agent crate. Operators may override these policies locally, but only to
revoke privileges.

## 1. Locations _(normative)_

- **Authoring (source):** `agents/<agent_name>/policy.caps`
- **Bundle (packaged):** `agent.bundle/policy.caps`
- **Operator overrides:** `~/.runloop/caps/overrides/<agent_id>.toml`

Overrides are applied before launch with intersection semantics (see §3).

## 2. TOML schema _(normative)_

```toml
[capabilities]
fs = ["/home/user/work/notes"]
net = ["api.mailprovider.com"]
time = true
kb_read = true
kb_write = ["contacts", "artifacts"]
secrets = ["runloop/mail/smtp_api_key"]
model = true
exec = false
```

Supported keys:

<!-- markdownlint-disable MD013 -->

| Key        | Type                          | Default | Notes                                                                                                                                                         |
| ---------- | ----------------------------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `fs`       | array of absolute directories | `[]`    | Directories only; file paths are ignored. Paths are normalized; globs not allowed in v0.1.                                                                    |
| `net`      | array of hostnames            | `[]`    | Hostnames may include port (`example.com:443`). Wildcards arrive in v0.2.                                                                                     |
| `time`     | bool                          | `false` | Grants access to monotonic/UTC clocks.                                                                                                                        |
| `kb_read`  | bool or array                 | `false` | `true` grants read-only access to all domains; array limits to subset (`["contacts"]`). The `kb_read.contacts_raw` grant lifts KB redaction for PII (emails). |
| `kb_write` | bool or array                 | `false` | Same semantics as `kb_read`; `true` is discouraged outside trusted agents.                                                                                    |
| `secrets`  | array of secret IDs           | `[]`    | IDs follow `runloop/<scope>/<name>`.                                                                                                                          |
| `model`    | bool                          | `false` | Allows requests to model broker.                                                                                                                              |
| `exec`     | bool                          | `false` | Command execution on host; off by default and should be granted sparingly.                                                                                    |

<!-- markdownlint-enable MD013 -->

Shorthand booleans (`kb_read = true`) expand to the full domain set at load
time. Internally the runtime converts the configuration into bitsets per
capability family.

## 3. Override semantics _(normative)_

- Base policy: union of bundle `policy.caps` and agent manifest requirements.
- Operator override is loaded (if present). Effective capabilities =
  **intersection**(`base`, `override`).
- Missing override keys imply no additional restriction. Setting a key to an
  empty list or `false` revokes the capability entirely.
- Overrides cannot **grant** capabilities absent from base; attempting to do so
  logs a warning and the extra entries are ignored.
- When the **effective** capabilities resolve to empty (no fs/net/time/kb/model/
  secrets/exec grants), the runtime still launches the agent but records a
  `cap.audit` entry with `cap="caps.empty"`, `op="_start"`, and
  `reason="caps_empty"` so operators understand the agent will be inert until
  policy is relaxed. Hostcalls will continue to be denied under
  `audit_on_deny=true`.

Example override (`~/.runloop/caps/overrides/writer.toml`):

```toml
[capabilities]
net = []                # revoke network access entirely
kb_write = ["drafts"]   # restrict writes to the drafts domain
```

## 4. Validation & tooling _(informative)_

- `rlp caps lint` (planned) will validate TOML against the schema and show diff
  vs. base policy.
- During agent launch, Runloop logs both base and effective capability sets for
  auditing.
- Capability decisions are surfaced in structured logs (`cap_allow` / `cap_deny`
  events) and, when enabled via `security.caps.audit_on_*`, persisted to the
  knowledge base as `cap.audit` events for replay.

## 5. Future additions

- Wildcard hostnames with verification (`*.example.com`).
- Rate limits (`model.tokens_per_minute`).
- Capability templates shared across agents.
- Optional hardening flag (`security.caps.fail_on_empty`) to fail-fast when
  effective capabilities are empty instead of launching an inert agent.
