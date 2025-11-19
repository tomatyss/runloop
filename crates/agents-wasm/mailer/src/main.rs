use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use clap::Parser;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    topic: String,
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

fn send(draft: &DraftInput, contact: &ContactInput, topic: &str) -> MailerOutput {
    let delivered_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    MailerOutput {
        status: "sent".into(),
        recipients: vec![contact.email.clone()],
        topic: topic.to_string(),
        message_id: format!("msg-{}", Uuid::new_v4().simple()),
        delivered_at_ms,
        body_preview: Some(draft.body_md.lines().take(8).collect::<Vec<_>>().join("\n")),
    }
}
