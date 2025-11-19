# Tool Attachments (`tools.json`)

> **Status:** Draft (normative for bundle layout once tooling lands). Defines
> how agent bundles describe optional host-provided tools so the runtime,
> router, and CLI wizard share the same contract.

Runloop agents may delegate specific actions to host tools (CLI binaries, HTTP
APIs, etc.). These attachments are declared in a `tools.json` file that lives
next to `manifest.toml` and `policy.caps` inside each bundle
(`agents/<name>/tools.json` in source, `agent.bundle/tools.json` after
packaging). Bundles are signed via `manifest.toml`, so the manifest must list
the BLAKE3 digest of `tools.json` just like other artifacts (see `docs/ops.md`).
Consumers treat the JSON document as the single source of truth for tool
metadata; no new `[tools]` tables are added to the manifest.

## 1. Top-level structure

```json
{
  "version": 1,
  "tools": [{ "...": "see §2" }]
}
```

- `version` — schema version of this document (currently `1`).
- `tools` — ordered list of attachments available to the agent. Missing or empty
  means the agent has no attachments.

## 2. Tool entry schema

- `id` (string, required) – stable identifier such as `mail.smtp_send`. Payload
  frames reference this name.
- `description` (string, required) – short human-readable summary.
- `transport.kind` (enum, required) – either `exec` or `http`. Additional
  transport fields depend on the selected kind:
  - `exec` requires `transport.command` (absolute path) and optionally
    `transport.args` (extra CLI args appended after the JSON blob path).
  - `http` requires `transport.method` (`GET`, `POST`, etc.), `transport.url`
    (absolute URL, templating allowed), and optional `transport.headers` (static
    header map; values may include secret placeholders).
- `input_schema` (JSON Schema or `$ref`, required) – describes the
  `CT_TOOL_CALL` payload so validation can happen before execution.
- `result_schema` (JSON Schema or `$ref`, required) – shape of the
  `CT_TOOL_RESULT` payload stored in the KB.
- `schema_refs` (object, optional) – helper map for additional schema IDs used
  inside this entry.
- `capabilities` (array of strings, optional) – capability families required
  before exposing the tool (for example `net:api.mail.io`).
- `secrets` (array of strings, optional) – secret IDs resolved prior to
  invocation.
- `budget.tokens` / `budget.usd` (optional) – per-tool budget hints enforced by
  the runtime.
- `requires_confirmation` (bool, optional) – defaults to `false`. When set, tool
  invocations inherit `confirm_external_actions`.
- `observability.tags` (array of strings, optional) – tags appended to trace or
  audit records for filtering.

### JSON example

```json
{
  "version": 1,
  "tools": [
    {
      "id": "mail.smtp_send",
      "description": "Send rendered drafts via the configured SMTP relay.",
      "transport": {
        "kind": "http",
        "method": "POST",
        "url": "https://api.mail.example/send"
      },
      "input_schema": {
        "$ref": "schema:mail.send.request.v1"
      },
      "result_schema": {
        "$ref": "schema:mail.send.result.v1"
      },
      "capabilities": ["net:api.mail.example"],
      "secrets": ["runloop/mail/smtp_api_key"],
      "requires_confirmation": true,
      "observability": {
        "tags": ["external", "mail"]
      }
    }
  ]
}
```

## 3. Runtime behavior

- **Capability enforcement.** Before exposing a tool, the runtime intersects the
  tool’s required capability families with the agent’s effective `policy.caps`.
  Any missing grant denies the attachment outright.
- **Secrets.** Secret identifiers listed in `secrets[]` are resolved via the
  configured `SecretResolver` and injected into environment variables or HTTP
  headers depending on the transport. Failures emit `cap_denied` audit events.
- **Confirmation gate.** When `requires_confirmation` is `true`, invocations
  behave like other external actions: the CLI/TUI prompts the operator unless
  `security.confirm_external_actions` is disabled.
- **Tracing + replay.** Tool calls emit `CT_TOOL_CALL` / `CT_TOOL_RESULT` frames
  with the declared `id` and schema references. Payloads are persisted in the KB
  and included in run traces so `rlp replay` can rehydrate the tool outputs
  deterministically (or flag missing tools).

## 4. Wizard & authoring guidance

- `rlp agent scaffold` should generate an empty `tools.json` (version 1) and
  offer to add entries interactively. Documented defaults make the wizard
  predictable and lintable.
- Authors can share schema references by pointing `input_schema`/`result_schema`
  at existing IDs (see `docs/rmp-registry.md`). Inline schemas are also allowed
  but should stay concise; large definitions belong in shared files that the
  wizard references via `$ref`.
- Keep `id` values stable. They become part of the KB provenance and any change
  should be treated as a breaking update.

Future revisions can introduce new `transport.kind` variants (e.g., `unix`
socket) via a new `version` number. Older runtimes must reject unsupported
versions so the CLI can guide authors to downgrade or install newer binaries.
