# contact_resolver

**Input (Intent):**

```json
{
  "type": "resolve_contact",
  "name_or_hint": "John",
  "email_hint": null,
  "context_tags": ["q4"]
}
```

**Output (Artifact):**

```json
{ "type":"contact_candidates",
  "candidates":[ { "id":"uuid", "name":"John Smith","email":"john@acme.com","confidence":0.86 } ],
  "provenance":[<event_ids>]
}
```

**Capabilities:** kb.read.contacts, kb.search, kb.write.contacts? (for confirmed
upsert), model (optional for disambiguation)  
**Timeout/budget:** 2s / 500 tokens
