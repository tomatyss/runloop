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
#[command(about = "Runloop writer agent (wasm32-wasip1)")]
struct Cli {
    #[arg(long)]
    recipient_base64: String,
    #[arg(long)]
    context_base64: Option<String>,
    #[arg(long)]
    topic: String,
    #[arg(long, default_value = "neutral-friendly")]
    tone: String,
}

#[derive(Debug, Deserialize)]
struct Recipient {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContextInput {
    #[serde(default)]
    snippets: Vec<SnippetInput>,
}

#[derive(Debug, Deserialize)]
struct SnippetInput {
    #[serde(default)]
    excerpt: Option<String>,
    #[serde(default)]
    event_id: Option<i64>,
}

#[derive(Debug, Serialize)]
struct WriterOutput {
    body_md: String,
    rationale: String,
    citations: Vec<i64>,
    word_count: usize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    host::signal_ready();
    let recipient: Recipient = decode_json(&cli.recipient_base64, "recipient")?;
    let context: Option<ContextInput> = cli
        .context_base64
        .as_deref()
        .map(|value| decode_json(value, "context"))
        .transpose()?;
    let output = render(&recipient, context.as_ref(), &cli.topic, &cli.tone);
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn decode_json<T: for<'de> Deserialize<'de>>(encoded: &str, label: &str) -> Result<T> {
    let bytes = BASE64
        .decode(encoded)
        .with_context(|| format!("failed to decode {label} payload"))?;
    serde_json::from_slice(&bytes).with_context(|| format!("invalid {label} JSON"))
}

fn render(
    recipient: &Recipient,
    context: Option<&ContextInput>,
    topic: &str,
    tone: &str,
) -> WriterOutput {
    let greeting = match tone {
        "formal" => "Hello",
        "enthusiastic" => "Hey",
        _ => "Hi",
    };
    let name = recipient
        .name
        .as_deref()
        .filter(|n| !n.is_empty())
        .unwrap_or("there");
    let mut lines = Vec::new();
    lines.push(format!("{greeting} {name},"));
    lines.push(String::new());
    lines.push(format!("I wanted to follow up about {topic}."));
    if let Some(bundle) = context {
        let snippets: Vec<&SnippetInput> = bundle
            .snippets
            .iter()
            .filter(|snip| {
                snip.excerpt
                    .as_deref()
                    .map(|s| !s.is_empty())
                    .unwrap_or(false)
            })
            .collect();
        if !snippets.is_empty() {
            lines.push(String::new());
            lines.push("Key updates:".into());
            for snippet in snippets {
                lines.push(format!("- {}", snippet.excerpt.as_deref().unwrap_or("n/a")));
            }
        }
    }
    lines.push(String::new());
    lines.push("Let me know if you have any questions or if you'd like to chat more.".into());
    lines.push(String::new());
    lines.push("Best,".into());
    lines.push("Runloop Agent".into());
    let body = lines.join("\n");
    let citations = context
        .map(|bundle| {
            bundle
                .snippets
                .iter()
                .filter_map(|snippet| snippet.event_id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    WriterOutput {
        word_count: body.split_whitespace().count(),
        rationale: format!("Draft generated for {} about {topic}", name),
        citations,
        body_md: body,
    }
}
