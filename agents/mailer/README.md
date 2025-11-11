# mailer

**Input (Artifact + Intent):**

```json
{
  "type": "send_email",
  "draft_path": "blob:sha256:...",
  "to": "john@acme.com",
  "confirm": true
}
```

**Output (StateDelta):**

```json
{
  "event": "email.sent",
  "payload": { "to": ["john@acme.com"], "subject": "...", "artifact_id": "..." }
}
```

**Capabilities:** net(api.mailprovider.com), secrets(mail.smtp.key),
kb.write.events  
**Timeout/budget:** 3s / 50 tokens  
**Notes:** must hard-require human confirmation unless policy overrides.
