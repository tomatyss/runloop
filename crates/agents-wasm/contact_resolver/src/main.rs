use std::borrow::Cow;

use anyhow::Result;
use clap::Parser;
use serde::Serialize;
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
#[command(about = "Runloop contact resolver (wasm32-wasip1)")]
struct Cli {
    /// Name or email hint to resolve.
    #[arg(long)]
    query: String,
}

#[derive(Clone, Debug, Serialize)]
struct ContactEntry<'a> {
    contact_id: Cow<'a, str>,
    name: Cow<'a, str>,
    email: Cow<'a, str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    org: Option<Cow<'a, str>>,
    confidence: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<Cow<'a, str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_event_id: Option<()>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    host::signal_ready();
    let contact = resolve(&cli.query)?;
    println!("{}", serde_json::to_string_pretty(&contact)?);
    Ok(())
}

fn resolve(query: &str) -> Result<ContactEntry<'static>> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        anyhow::bail!("--query must not be empty");
    }
    if let Some(contact) = CONTACTS.iter().find(|entry| {
        let needle = trimmed.to_ascii_lowercase();
        entry.name.to_ascii_lowercase().contains(&needle)
            || entry.email.to_ascii_lowercase().contains(&needle)
    }) {
        return Ok(contact.clone());
    }
    Ok(build_stub(trimmed))
}

fn build_stub(query: &str) -> ContactEntry<'static> {
    let identifier = slug(query).unwrap_or_else(|| Uuid::new_v4().to_string());
    let (name, email) = if query.contains('@') {
        let name = query.split('@').next().unwrap_or("unknown");
        (name.trim().to_string(), query.trim().to_ascii_lowercase())
    } else {
        (
            query.trim().to_string(),
            format!("{}@unknown.local", identifier),
        )
    };
    ContactEntry {
        contact_id: Cow::Owned(format!("contact:{identifier}")),
        name: Cow::Owned(title_case(&name)),
        email: Cow::Owned(email),
        org: None,
        confidence: 0.4,
        notes: Some(Cow::Borrowed("Auto-generated stub contact")),
        last_event_id: None,
    }
}

fn slug(value: &str) -> Option<String> {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_whitespace() || ch == '-' || ch == '_' {
            if !out.ends_with('-') {
                out.push('-');
            }
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn title_case(input: &str) -> String {
    input
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    let mut result = String::new();
                    result.push(first.to_ascii_uppercase());
                    result.push_str(chars.as_str());
                    result
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

static CONTACTS: &[ContactEntry<'static>] = &[
    ContactEntry {
        contact_id: Cow::Borrowed("contact:john-smith"),
        name: Cow::Borrowed("John Smith"),
        email: Cow::Borrowed("john@acme.com"),
        org: Some(Cow::Borrowed("Acme Corp")),
        confidence: 0.93,
        notes: Some(Cow::Borrowed("Primary stakeholder for Q4 planning.")),
        last_event_id: None,
    },
    ContactEntry {
        contact_id: Cow::Borrowed("contact:maria-lee"),
        name: Cow::Borrowed("Maria Lee"),
        email: Cow::Borrowed("maria@acme.com"),
        org: Some(Cow::Borrowed("Acme Corp")),
        confidence: 0.88,
        notes: Some(Cow::Borrowed("Finance lead for Acme.")),
        last_event_id: None,
    },
    ContactEntry {
        contact_id: Cow::Borrowed("contact:dev-rel"),
        name: Cow::Borrowed("Dev Rel Alias"),
        email: Cow::Borrowed("devrel@runloop.local"),
        org: Some(Cow::Borrowed("Runloop")),
        confidence: 0.65,
        notes: Some(Cow::Borrowed("Fallback auto-generated contact.")),
        last_event_id: None,
    },
];
