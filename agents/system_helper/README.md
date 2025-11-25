# system_helper

A helper agent that can:

- send a prompt to the model broker (Gemini by default), and/or
- run a host command and capture stdout/stderr.

**Inputs (`with`):**

- `prompt` — optional text to send to the configured model.
- `command` — optional shell command to execute on the host.
- `model` — model identifier for the broker (defaults to `gemini-1.5-flash` in the wasm binary).
- `output_cap` — buffer cap for captured stdout/stderr (defaults to 8192 in the wasm binary).

**Outputs (`artifact.system_helper.result.v1`):**

```json
{
  "model": { "text": "...", "meta": { /* provider metadata */ } },
  "exec": { "command": "tmux list-sessions", "exit_code": 0, "stdout": "...", "stderr": "" }
}
```

**Capabilities:** exec (host commands), model (broker), fs (~/.config, ~/.tmux.conf). Extend the FS list in a local override if the agent needs to touch other configs (e.g., mailcap).

**Model provider setup:** add an `http_gemini` provider to `~/.runloop/config.yaml` and export the API key. Example:

```yaml
models:
  broker:
    providers:
      - id: "gemini"
        kind: "http_gemini"
        base_url: "https://generativelanguage.googleapis.com"
        secret_id: "runloop/models/gemini"
    route:
      - pattern: "*"
        provider: "gemini"
```

Secrets resolve from the configured store or `RUNLOOP_MODELS_GEMINI` in the environment.
