# context_gatherer

**Input (Intent):**

```json
{ "type": "gather_context", "topic": "Q4 plan", "recipient_id": "uuid" }
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

## Matching semantics

- When assembling candidate snippets, the agent performs literal (escaped) searches over `payload_json`, always lowercasing both sides so topic/contact filters behave case-insensitively.
- SQL wildcard characters (`%`, `_`) supplied by users are escaped before issuing the query, preventing unintended broad matches or injection.
