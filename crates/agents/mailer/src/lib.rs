use runloop_agents_common::{
    ActionProposal, AgentContext, AgentError, AgentResult, MailResult, ResolvedContact, Review,
};
use runloop_core::ids::EventId;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MailRequest {
    pub draft: DraftData,
    pub contact: ResolvedContact,
    pub review: Review,
    pub topic: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DraftData {
    pub artifact_id: EventId,
    pub path: String,
    pub body_preview: String,
}

pub async fn send(ctx: &AgentContext, req: MailRequest) -> AgentResult<MailResult> {
    if !req.review.ok {
        return Err(AgentError::InvalidInput(
            "draft not approved by critic".into(),
        ));
    }
    let confirmation = ctx
        .confirmation()
        .ok_or(AgentError::ConfirmationUnavailable)?;
    let proposal = ActionProposal {
        id: format!("mail:{}", req.draft.artifact_id.0),
        trace_id: ctx.trace_id(),
        opening_id: ctx.opening_id(),
        agent: ctx.agent_id(),
        summary: format!("Send draft '{}' to {}", req.topic, req.contact.email),
        recipients: vec![req.contact.email.clone()],
        artifact_path: req.draft.path.clone().into(),
    };
    let decision = confirmation.confirm(proposal).await?;
    if !decision.approved {
        return Err(AgentError::ConfirmationDeclined);
    }
    println!("--- Dry-run mailer output ---");
    println!("To: {}", req.contact.email);
    println!("Subject: {}", req.topic);
    println!("{}", req.draft.body_preview);
    println!("-----------------------------");

    let payload = json!({
        "to": [req.contact.email],
        "subject": req.topic,
        "artifact_id": req.draft.artifact_id.0,
    });
    ctx.propose_event(
        "email.sent",
        payload,
        decision.rationale.clone().or(Some("mailer dry-run".into())),
    )?;
    Ok(MailResult {
        status: "dry-run".into(),
        recipients: vec![req.contact.email],
        artifact_id: req.draft.artifact_id,
    })
}
