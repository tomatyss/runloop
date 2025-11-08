use runloop_agents_common::{
    AgentContext, AgentResult, ContextBundle, ContextSnippet, ResolvedContact,
};
use runloop_core::ids::EventId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::info;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextRequest {
    pub topic: String,
    pub contact: Option<ResolvedContact>,
}

pub async fn gather(ctx: &AgentContext, req: ContextRequest) -> AgentResult<ContextBundle> {
    ctx.ensure_views()?;
    let snippets = collect_snippets(ctx, &req)?;
    Ok(ContextBundle {
        topic: req.topic.clone(),
        snippets,
    })
}

fn collect_snippets(ctx: &AgentContext, req: &ContextRequest) -> AgentResult<Vec<ContextSnippet>> {
    let mut clauses = Vec::new();
    if let Some(contact) = &req.contact {
        clauses.push(format!(
            "payload_json LIKE '%{}%'",
            escape_like(&contact.email)
        ));
        clauses.push(format!(
            "payload_json LIKE '%{}%'",
            escape_like(&contact.name.to_ascii_lowercase())
        ));
    }
    if !req.topic.is_empty() {
        clauses.push(format!(
            "payload_json LIKE '%{}%'",
            escape_like(&req.topic.to_ascii_lowercase())
        ));
    }
    let predicate = if clauses.is_empty() {
        "1=1".to_string()
    } else {
        clauses.join(" OR ")
    };
    let sql = format!(
        "SELECT id, kind, payload_json \
         FROM events \
         WHERE {predicate} \
         ORDER BY id DESC \
         LIMIT 5"
    );
    let rows = ctx.kb().query(&sql)?.rows;
    let mut snippets = Vec::new();
    for row in rows {
        if let Some(snippet) = row_to_snippet(row, &req.topic) {
            snippets.push(snippet);
        }
    }

    if snippets.is_empty() {
        let fallback = ContextSnippet {
            title: format!(
                "No prior events for {}",
                req.contact
                    .as_ref()
                    .map(|c| c.name.as_str())
                    .unwrap_or("recipient")
            ),
            excerpt: format!(
                "No recorded artifacts mention \"{}\" yet. Consider adding recent notes.",
                req.topic
            ),
            event_id: EventId(0),
        };
        info!("context gatherer using fallback snippet");
        snippets.push(fallback);
    }
    Ok(snippets)
}

fn row_to_snippet(row: Value, topic: &str) -> Option<ContextSnippet> {
    let obj = row.as_object()?;
    let id = obj.get("id")?.as_i64()?;
    let kind = obj.get("kind")?.as_str()?.to_string();
    let payload = obj.get("payload_json")?.as_str()?.to_string();
    let excerpt = summarise_payload(&payload, topic);
    Some(ContextSnippet {
        title: format!("{kind} event"),
        excerpt,
        event_id: EventId(id),
    })
}

fn summarise_payload(payload: &str, topic: &str) -> String {
    if payload.len() <= 120 {
        payload.to_string()
    } else if topic.is_empty() {
        format!("{}…", payload.chars().take(117).collect::<String>())
    } else if let Some(idx) = payload
        .to_ascii_lowercase()
        .find(&topic.to_ascii_lowercase())
    {
        let start = idx.saturating_sub(30);
        let end = (idx + topic.len() + 30).min(payload.len());
        format!("…{}…", &payload[start..end])
    } else {
        format!("{}…", payload.chars().take(117).collect::<String>())
    }
}

fn escape_like(input: &str) -> String {
    input.replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarise_payload_highlights_topic_window() {
        let payload = "The team discussed the Q4 plan and next steps during the sync.";
        let excerpt = summarise_payload(payload, "Q4");
        assert!(excerpt.contains("Q4"));
        assert!(excerpt.len() <= payload.len());
    }

    #[test]
    fn escape_like_doubles_quotes() {
        assert_eq!(escape_like("acme'co"), "acme''co");
    }
}
