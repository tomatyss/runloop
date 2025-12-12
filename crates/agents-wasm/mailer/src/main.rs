use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use clap::Parser;
use serde::{Deserialize, Serialize};

#[allow(unsafe_code)]
mod host {
    #[link(wasm_import_module = "runloop")]
    unsafe extern "C" {
        fn notify_ready();
    }

    pub(super) fn signal_ready() {
        // SAFETY: the host runtime injects `notify_ready` with no parameters,
        // so calling it cannot violate any memory safety invariants.
        unsafe { notify_ready() };
    }
}

#[derive(Parser, Debug)]
#[command(about = "Runloop mailer agent (wasm32-wasip1)")]
struct Cli {
    #[arg(long)]
    draft_base64: String,
    #[arg(long)]
    contact_base64: String,
    #[arg(long)]
    review_base64: String,
    #[arg(long)]
    topic: String,
}

#[derive(Debug, Deserialize)]
struct DraftInput {
    #[serde(default)]
    artifact_id: i64,
    #[serde(default)]
    body_md: String,
}

#[derive(Debug, Deserialize)]
struct ContactInput {
    #[serde(default)]
    email: String,
}

#[derive(Debug, Deserialize)]
struct ReviewInput {
    ok: bool,
}

#[derive(Debug, Serialize)]
struct MailerOutput {
    status: String,
    recipients: Vec<String>,
    message_id: String,
    delivered_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    body_preview: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    host::signal_ready();
    let draft: DraftInput = decode_json(&cli.draft_base64, "draft")?;
    let contact: ContactInput = decode_json(&cli.contact_base64, "contact")?;
    let review: ReviewInput = decode_json(&cli.review_base64, "review")?;
    if !review.ok {
        bail!("review rejected draft");
    }
    let output = send(&draft, &contact, &cli.topic);
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn decode_json<T: for<'de> Deserialize<'de>>(encoded: &str, label: &str) -> Result<T> {
    let bytes = BASE64
        .decode(encoded)
        .with_context(|| format!("failed to decode {label} payload"))?;
    serde_json::from_slice(&bytes).with_context(|| format!("invalid {label} JSON"))
}

fn send(draft: &DraftInput, contact: &ContactInput, _topic: &str) -> MailerOutput {
    let message_id = if draft.artifact_id != 0 {
        format!("dryrun-{}", draft.artifact_id)
    } else {
        "dryrun".into()
    };
    MailerOutput {
        status: "dry-run".into(),
        recipients: vec![contact.email.clone()],
        message_id,
        delivered_at_ms: 0,
        body_preview: Some(draft.body_md.lines().take(8).collect::<Vec<_>>().join("\n")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(body: &str, artifact_id: i64) -> DraftInput {
        DraftInput {
            artifact_id,
            body_md: body.into(),
        }
    }

    #[test]
    fn includes_delivery_metadata() {
        let draft = draft("hello", 42);
        let contact = ContactInput {
            email: "test@example.com".into(),
        };
        let mail = send(&draft, &contact, "topic");
        assert_eq!(mail.status, "dry-run");
        assert_eq!(mail.message_id, "dryrun-42");
        assert_eq!(mail.delivered_at_ms, 0);
        assert!(mail.body_preview.as_ref().is_some_and(|p| p.contains("hello")));
    }
}
