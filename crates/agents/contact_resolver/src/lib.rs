use runloop_agents_common::{
    AgentContext, AgentError, AgentResult, ResolvedContact, contact_payload,
};
use runloop_core::ids::EventId;
use runloop_kb::{derive_contact_key, normalize_email};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContactIntent {
    pub recipient_query: String,
}

pub async fn resolve(ctx: &AgentContext, intent: ContactIntent) -> AgentResult<ResolvedContact> {
    ctx.ensure_views()?;
    if let Some(contact) = lookup_contact(ctx, intent.recipient_query.trim())? {
        return Ok(contact);
    }

    let stub = create_stub_contact(ctx, intent.recipient_query.trim())?;
    warn!(
        contact = %stub.contact_id,
        "contact missing; created stub with confidence {:.2}. Please confirm.",
        stub.confidence
    );
    Ok(stub)
}

fn lookup_contact(ctx: &AgentContext, query: &str) -> AgentResult<Option<ResolvedContact>> {
    if query.is_empty() {
        return Ok(None);
    }
    let pattern = format!("%{}%", escape_like(&query.to_ascii_lowercase()));
    let sql = format!(
        "SELECT contact_key, name, email, org, trust, source_event \
         FROM contacts \
         WHERE lower(name) LIKE '{pattern}' \
            OR lower(email) LIKE '{pattern}' \
         ORDER BY trust DESC, source_event DESC \
         LIMIT 5"
    );
    let rows = ctx.kb().query(&sql)?.rows;
    let mut best: Option<ResolvedContact> = None;
    let mut best_score = 0.0f32;

    for row in rows {
        if let Some(contact) = row_to_contact(row) {
            let score = score_contact(&contact, query);
            if score > best_score {
                best_score = score;
                best = Some(contact);
            }
        }
    }
    Ok(best)
}

fn score_contact(contact: &ResolvedContact, query: &str) -> f32 {
    let mut score = contact.confidence;
    if contact.email.eq_ignore_ascii_case(query.trim()) {
        score += 0.3;
    } else if contact
        .name
        .to_ascii_lowercase()
        .contains(&query.to_ascii_lowercase())
    {
        score += 0.1;
    }
    score
}

fn row_to_contact(row: Value) -> Option<ResolvedContact> {
    let obj = row.as_object()?;
    let key = obj.get("contact_key")?.as_str()?.to_string();
    let name = obj
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let email = obj.get("email").and_then(Value::as_str).unwrap_or_default();
    if email.is_empty() {
        return None;
    }
    let org = obj
        .get("org")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let trust = obj.get("trust").and_then(Value::as_f64).unwrap_or(0.5) as f32;
    let event_id = obj.get("source_event").and_then(Value::as_i64).map(EventId);

    Some(ResolvedContact {
        contact_id: format!("contact:{key}"),
        name,
        email: email.to_string(),
        org,
        confidence: trust.min(1.0),
        last_event_id: event_id,
    })
}

fn create_stub_contact(ctx: &AgentContext, query: &str) -> AgentResult<ResolvedContact> {
    let (name, email) = if query.contains('@') {
        ("Unknown".to_string(), normalize_email(query))
    } else {
        (
            capitalize_words(query),
            format!("{}@unknown.local", slug(query)),
        )
    };

    let contact_key = derive_contact_key(Some(&email), Some(&name), None)
        .ok_or_else(|| AgentError::InvalidInput("unable to derive contact key".into()))?;
    let contact = ResolvedContact {
        contact_id: format!("contact:{contact_key}"),
        name,
        email,
        org: None,
        confidence: 0.4,
        last_event_id: None,
    };
    let payload = contact_payload(&contact, 0.2);
    let rationale = format!("stub created for query '{query}'");
    ctx.propose_event("contact.upserted", payload, Some(rationale))?;
    Ok(contact)
}

fn escape_like(input: &str) -> String {
    input.replace('\'', "''")
}

fn slug(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn capitalize_words(input: &str) -> String {
    input
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use runloop_agents_common::AgentContext;
    use runloop_core::ids::{AgentId, OpeningId, TraceId};
    use runloop_kb::{KnowledgeBase, Materializer, Provenance, StateDelta};
    use serde_json::json;

    fn ctx() -> AgentContext {
        let kb = KnowledgeBase::new();
        let ctx = AgentContext::new(
            kb.clone(),
            None,
            PathBuf::from("/tmp"),
            TraceId::new(),
            OpeningId::new(),
            AgentId::new(),
            None,
        );
        let provenance = Provenance {
            trace_id: ctx.trace_id().to_string(),
            opening_id: ctx.opening_id().to_string(),
            agent_id: ctx.agent_id().to_string(),
            inputs_hash: None,
            rationale: Some("seed".into()),
        };
        let payload = json!({
            "name": "John Smith",
            "email": "john@acme.com",
            "org": "Acme",
            "trust": 0.9,
            "evidence": []
        });
        kb.propose(StateDelta::new(
            "contact.upserted",
            ctx.agent_id().to_string(),
            Some("user".to_string()),
            payload,
            provenance,
        ))
        .expect("seed contact");
        let materializer = Materializer::new(kb.clone());
        loop {
            if !materializer.sync().expect("materialize") {
                break;
            }
        }
        ctx
    }

    use std::path::PathBuf;

    #[tokio::test]
    async fn resolves_seed_contact() {
        let ctx = ctx();
        let result = resolve(
            &ctx,
            ContactIntent {
                recipient_query: "John".into(),
            },
        )
        .await
        .expect("resolver");
        assert_eq!(result.email, "john@acme.com");
    }
}
