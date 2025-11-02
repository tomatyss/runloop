# writer

**Input (Intent):**
```json
{ "type":"draft_email",
  "recipient":{"name":"John Smith","email":"john@acme.com"},
  "topic":"Q4 plan",
  "context_snippets":[...],
  "tone":"neutral-friendly",
  "length_words":[120,180]
}
```

**Output (Artifact):**
```json
{ "type":"draft_email.md", "body_md":"...", "rationale":"...", "citations":[<event_ids>] }
```

**Capabilities:** model, kb.write.artifacts  
**Timeout/budget:** 8s / 1,600 tokens
