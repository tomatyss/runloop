#!/usr/bin/env bash
# Validate the temporary native agent shims by running the compose_email chain.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required to exercise the agent shims." >&2
  exit 1
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

CONTACT_JSON="$TMP_DIR/contact.json"
CONTEXT_JSON="$TMP_DIR/context.json"
DRAFT_JSON="$TMP_DIR/draft.json"
REVIEW_JSON="$TMP_DIR/review.json"
SEND_JSON="$TMP_DIR/send.json"
DRAFTS_DIR="$TMP_DIR/drafts"

"$ROOT/agents/contact_resolver/bin/contact_resolver" --query "john" >"$CONTACT_JSON"
"$ROOT/agents/context_gatherer/bin/context_gatherer" \
  --topic "Q4 plan" \
  --contact-json "$CONTACT_JSON" \
  >"$CONTEXT_JSON"
"$ROOT/agents/writer/bin/writer" \
  --recipient-json "$CONTACT_JSON" \
  --context-json "$CONTEXT_JSON" \
  --topic "Q4 plan" \
  --output-dir "$DRAFTS_DIR" \
  >"$DRAFT_JSON"
"$ROOT/agents/critic/bin/critic" --draft-json "$DRAFT_JSON" >"$REVIEW_JSON"
"$ROOT/agents/mailer/bin/mailer" \
  --draft-json "$DRAFT_JSON" \
  --contact-json "$CONTACT_JSON" \
  --topic "Q4 plan" \
  >"$SEND_JSON"

python3 - "$CONTACT_JSON" "$CONTEXT_JSON" "$DRAFT_JSON" "$REVIEW_JSON" "$SEND_JSON" <<'PY'
import json
import pathlib
import sys

contact, context, draft, review, send = [
    json.load(open(path, "r", encoding="utf-8")) for path in sys.argv[1:6]
]

assert contact["email"] == "john@acme.com", "contact resolver returned unexpected email"
assert context["topic"] == "Q4 plan", "context topic mismatch"
assert len(context["snippets"]) >= 2, "context should include snippets"
assert pathlib.Path(draft["path"]).is_file(), "draft markdown missing on disk"
assert isinstance(review["ok"], bool), "critic output missing ok flag"
assert send["status"] == "sent", "mailer should report sent status"
print("agent shims ok")
PY
