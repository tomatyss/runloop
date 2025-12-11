use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use clap::Parser;
use serde::{Deserialize, Serialize};

const MIN_WORDS: usize = 10;
const MAX_WORDS: usize = 180;

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
    let draft: DraftArtifact = decode_json(&cli.draft_base64, "draft")?;
    let review = critique(&draft);
    println!("{}", serde_json::to_string_pretty(&review)?);
    Ok(())
}

fn decode_json<T: for<'de> Deserialize<'de>>(encoded: &str, label: &str) -> Result<T> {
    let bytes = BASE64
        .decode(encoded)
        .with_context(|| format!("failed to decode {label} payload"))?;
    serde_json::from_slice(&bytes).with_context(|| format!("invalid {label} JSON"))
}

#[derive(Debug, Deserialize)]
struct DraftArtifact {
    #[serde(default)]
    body_md: String,
    #[serde(default)]
    word_count: usize,
}

fn critique(draft: &DraftArtifact) -> CriticOutput {
    let mut suggestions = Vec::new();
    let mut blocking = false;

    let word_count = effective_word_count(&draft.body_md, draft.word_count);

    if word_count < MIN_WORDS {
        blocking = true;
        suggestions.push(format!(
            "Add more detail so the draft reaches at least {MIN_WORDS} words (currently {}).",
            word_count
        ));
    }
    if word_count > MAX_WORDS {
        blocking = true;
        suggestions.push(format!(
            "Trim the draft to at most {MAX_WORDS} words (currently {}).",
            word_count
        ));
    }
    if !has_thank_you(&draft.body_md) {
        suggestions.push("Consider closing with a courteous thank-you.".into());
    }

    let ok = !blocking;
    let summary = match (ok, suggestions.is_empty()) {
        (true, true) => "Draft looks good.",
        (true, false) => "Draft looks good with minor polish.",
        (false, _) => "Draft needs attention.",
    };

    CriticOutput {
        ok,
        notes: ReviewNotes {
            summary: summary.into(),
            suggestions,
        },
    }
}

fn effective_word_count(body: &str, reported: usize) -> usize {
    if reported > 0 {
        reported
    } else {
        body.split_whitespace().count()
    }
}

fn has_thank_you(body: &str) -> bool {
    let body_lower = body.to_ascii_lowercase();
    body_lower.contains("thanks")
        || body_lower.contains("thank you")
        || body_lower.contains("thank-you")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(body: &str) -> DraftArtifact {
        DraftArtifact {
            body_md: body.into(),
            word_count: body.split_whitespace().count(),
        }
    }

    #[test]
    fn marks_overlong_as_blocking() {
        let body = "word ".repeat(200);
        let review = critique(&draft(&body));
        assert!(!review.ok);
        assert_eq!(review.notes.summary, "Draft needs attention.");
        assert!(
            review
                .notes
                .suggestions
                .iter()
                .any(|s| s.contains("Trim the draft"))
        );
    }

    #[test]
    fn missing_thank_you_is_non_blocking() {
        let body = "hello ".repeat(25);
        let review = critique(&draft(&body));
        assert!(review.ok);
        assert_eq!(
            review.notes.summary,
            "Draft looks good with minor polish."
        );
        assert!(
            review
                .notes
                .suggestions
                .iter()
                .any(|s| s.contains("thank-you"))
        );
    }

    #[test]
    fn blocks_too_short_draft() {
        let review = critique(&draft("Hello there"));
        assert!(!review.ok);
        assert_eq!(review.notes.summary, "Draft needs attention.");
        assert!(
            review
                .notes
                .suggestions
                .iter()
                .any(|s| s.contains("at least"))
        );
    }

    #[test]
    fn approves_clean_draft() {
        let body =
            "thanks for reviewing this draft. it covers the requested updates and next steps we discussed.".repeat(2);
        let review = critique(&draft(&body));
        assert!(review.ok);
        assert_eq!(review.notes.summary, "Draft looks good.");
        assert!(review.notes.suggestions.is_empty());
    }
}
