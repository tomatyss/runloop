use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
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
#[command(about = "Runloop context gatherer (wasm32-wasip1)")]
struct Cli {
    #[arg(long)]
    topic: String,
    #[arg(long)]
    contact_base64: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Contact {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Serialize)]
struct Snippet {
    title: String,
    excerpt: String,
    event_id: i64,
}

#[derive(Debug, Serialize)]
struct ContextBundle {
    topic: String,
    snippets: Vec<Snippet>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generated_at_ms: Option<u64>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    host::signal_ready();
    let contact = cli
        .contact_base64
        .as_deref()
        .map(decode_contact)
        .transpose()?
        .flatten();
    let bundle = build_context(&cli.topic, contact.as_ref());
    println!("{}", serde_json::to_string_pretty(&bundle)?);
    Ok(())
}

fn decode_contact(encoded: &str) -> Result<Option<Contact>> {
    let bytes = BASE64
        .decode(encoded)
        .context("failed to decode contact payload")?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let contact: Contact = serde_json::from_slice(&bytes).context("invalid contact JSON")?;
    Ok(Some(contact))
}

fn build_context(topic: &str, contact: Option<&Contact>) -> ContextBundle {
    let mut snippets = vec![
        Snippet {
            title: format!("{topic} planning sync"),
            excerpt: format!(
                "Team aligned on {} milestones during the weekly sync.",
                topic
            ),
            event_id: 1,
        },
        Snippet {
            title: "Action items".into(),
            excerpt: "Finalize roadmap draft and confirm headcount adjustments.".into(),
            event_id: 2,
        },
    ];
    if let Some(name) = contact.and_then(|c| c.name.as_deref()) {
        snippets.push(Snippet {
            title: "Relationship note".into(),
            excerpt: format!("{name} prefers concise status updates."),
            event_id: 3,
        });
    }
    let generated_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64);
    ContextBundle {
        topic: topic.to_string(),
        snippets,
        generated_at_ms,
    }
}
