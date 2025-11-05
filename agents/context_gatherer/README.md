# context_gatherer

**Input (Intent):**

```json
{ "type":"gather_context", "topic":"Q4 plan", "recipient_id":"uuid" }
```

**Output (Artifact):**

```json
{ "type":"context_bundle",
  "snippets":[{ "artifact_id":"...", "excerpt":"...", "source":"kb://artifacts/..." }],
  "citations":[<event_ids>]
}
```

**Capabilities:** kb.search, kb.read.artifacts, model (for summarization)  
**Timeout/budget:** 4s / 800 tokens
