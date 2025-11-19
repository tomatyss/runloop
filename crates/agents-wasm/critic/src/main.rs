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
#[command(about = "Runloop critic agent (wasm32-wasip1)")]
struct Cli {
    #[arg(long)]
    draft_base64: String,
}

#[derive(Debug, Deserialize)]
struct DraftInput {
    #[serde(default)]
    body_md: String,
}

#[derive(Debug, Serialize)]
struct CriticOutput {
    ok: bool,
    notes: ReviewNotes,
}

#[derive(Debug, Serialize)]
struct ReviewNotes {
    summary: String,
    #[serde(default)]
    suggestions: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    host::signal_ready();
    let draft: DraftInput = decode_json(&cli.draft_base64, "draft")?;
    let review = critique(&draft.body_md);
    println!("{}", serde_json::to_string_pretty(&review)?);
    Ok(())
}

fn decode_json<T: for<'de> Deserialize<'de>>(encoded: &str, label: &str) -> Result<T> {
    let bytes = BASE64
        .decode(encoded)
        .with_context(|| format!("failed to decode {label} payload"))?;
    serde_json::from_slice(&bytes).with_context(|| format!("invalid {label} JSON"))
}

fn critique(body: &str) -> CriticOutput {
    let mut suggestions = Vec::new();
    if body.split_whitespace().count() < 40 {
        suggestions.push("Add more detail so the recipient has clear next steps.".into());
    }
    if !body.to_ascii_lowercase().contains("follow up") {
        suggestions.push("Explicitly mention that this is a follow-up on the topic.".into());
    }
    let ok = suggestions.is_empty();
    let summary = if ok {
        "Draft looks good overall."
    } else {
        "Draft needs adjustments."
    };
    CriticOutput {
        ok,
        notes: ReviewNotes {
            summary: summary.into(),
            suggestions,
        },
    }
}
