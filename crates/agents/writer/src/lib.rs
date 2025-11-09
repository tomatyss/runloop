use std::fs;

use runloop_agents_common::{
    AgentContext, AgentResult, ContextBundle, DraftArtifact, ResolvedContact,
};
use runloop_core::ids::EventId;
use runloop_model_broker::{ModelOutput, ModelRequest};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DraftRequest {
    pub recipient: ResolvedContact,
    pub topic: String,
    pub context: ContextBundle,
    pub tone: Option<String>,
    pub length_hint: Option<(u32, u32)>,
    pub model: Option<String>,
    pub max_words: Option<usize>,
}

pub async fn draft(ctx: &AgentContext, req: DraftRequest) -> AgentResult<DraftArtifact> {
    let prompt = render_prompt(&req);
    let model_request = ModelRequest {
        trace_id: ctx.trace_id(),
        model: req.model.clone().unwrap_or_else(|| "null:compose".into()),
        prompt,
        params: None,
        budget_tokens: Some(4_000),
        timeout_ms: Some(10_000),
        cache_ttl_ms: Some(60_000),
        cache_key: None,
        stream: false,
    };

    let completion = match ctx.complete_model(model_request).await {
        Ok(output) => output,
        Err(err) => {
            warn!("model completion failed: {err}; falling back to heuristic template");
            fallback_output(&req)
        }
    };

    let mut body = completion.text.trim().to_string();
    let limit = req.max_words.unwrap_or(180);
    if word_count(&body) > limit {
        body = trim_to_words(&body, limit);
    }

    let rationale = format!(
        "draft generated for {} about {}",
        req.recipient.name, req.topic
    );
    let citations = req
        .context
        .snippets
        .iter()
        .map(|snippet| snippet.event_id)
        .collect::<Vec<_>>();

    let artifact = persist_artifact(ctx, &req.recipient, &body, &rationale, citations)?;
    Ok(artifact)
}

fn persist_artifact(
    ctx: &AgentContext,
    recipient: &ResolvedContact,
    body: &str,
    rationale: &str,
    citations: Vec<EventId>,
) -> AgentResult<DraftArtifact> {
    let drafts_dir = ctx.artifacts_dir().join("drafts");
    fs::create_dir_all(&drafts_dir)?;
    let filename = format!("draft-{}.md", Uuid::new_v4());
    let path = drafts_dir.join(filename);
    fs::write(&path, body)?;
    let sha256 = format!("{:x}", Sha256::digest(body.as_bytes()));

    let payload = serde_json::json!({
        "kind": "draft_email.md",
        "path": path.to_string_lossy(),
        "sha256": sha256,
        "summary": format!("Draft email to {}", recipient.name),
        "citations": citations.iter().map(|id| id.0).collect::<Vec<_>>(),
    });
    let event_id = ctx.propose_event("artifact.created", payload, Some(rationale.to_string()))?;

    Ok(DraftArtifact {
        artifact_id: event_id,
        path,
        sha256,
        body_md: body.to_string(),
        rationale: rationale.to_string(),
        citations,
        word_count: word_count(body),
    })
}

fn render_prompt(req: &DraftRequest) -> String {
    let tone = req.tone.as_deref().unwrap_or("neutral-friendly");
    let mut prompt = format!(
        "Write an email to {name} ({email}) about \"{topic}\". Tone: {tone}.",
        name = req.recipient.name,
        email = req.recipient.email,
        topic = req.topic,
        tone = tone
    );
    if let Some((min, max)) = req.length_hint {
        prompt.push_str(&format!(" Target length: {min}-{max} words."));
    } else {
        prompt.push_str(" Target length: under 180 words.");
    }
    if !req.context.snippets.is_empty() {
        prompt.push_str("\nContext bullets:\n");
        for snippet in &req.context.snippets {
            prompt.push_str(&format!("- {}\n", snippet.excerpt));
        }
    }
    prompt
}

fn fallback_output(req: &DraftRequest) -> ModelOutput {
    let mut body = String::new();
    body.push_str(&format!("Hi {},\n\n", req.recipient.name));
    body.push_str(&format!(
        "I wanted to follow up about {}. ",
        req.topic.to_lowercase()
    ));
    if let Some(snippet) = req.context.snippets.first() {
        body.push_str(&format!("Recently, {}. ", snippet.excerpt));
    }
    body.push_str(
        "Let me know if you have any questions or if you'd like to chat more.\n\nBest,\nRunloop\n",
    );
    ModelOutput {
        text: body,
        tokens_in: None,
        tokens_out: None,
        cached: false,
        provider: "fallback".into(),
        provider_model: "template".into(),
        latency_ms: 0,
        finish_reason: Some("fallback".into()),
    }
}

fn word_count(body: &str) -> usize {
    body.split_whitespace().count()
}

fn trim_to_words(body: &str, limit: usize) -> String {
    body.split_whitespace()
        .take(limit)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use runloop_agents_common::ContextSnippet;

    fn sample_request() -> DraftRequest {
        DraftRequest {
            recipient: ResolvedContact {
                contact_id: "contact:test".into(),
                name: "Alex".into(),
                email: "alex@example.com".into(),
                org: None,
                confidence: 0.9,
                last_event_id: None,
            },
            topic: "Roadmap".into(),
            context: ContextBundle {
                topic: "Roadmap".into(),
                snippets: vec![ContextSnippet {
                    title: "Update".into(),
                    excerpt: "Shipped auth".into(),
                    event_id: EventId(1),
                }],
            },
            tone: Some("upbeat".into()),
            length_hint: Some((50, 80)),
            model: None,
            max_words: Some(180),
        }
    }

    #[test]
    fn trim_to_words_enforces_limit() {
        let body = "one two three four five";
        assert_eq!(trim_to_words(body, 3), "one two three");
    }

    #[test]
    fn render_prompt_includes_context_bullets() {
        let prompt = render_prompt(&sample_request());
        assert!(prompt.contains("Target length: 50-80 words."));
        assert!(prompt.contains("- Shipped auth"));
        assert!(prompt.contains("Tone: upbeat"));
    }
}
