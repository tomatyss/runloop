use runloop_agents_common::{AgentResult, DraftArtifact, Review, ReviewNotes};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub draft: DraftArtifact,
}

pub async fn critique(req: ReviewRequest) -> AgentResult<Review> {
    let mut ok = true;
    let mut suggestions = Vec::new();

    if req.draft.word_count > 180 {
        ok = false;
        suggestions.push(format!(
            "Trim the draft to at most 180 words (currently {}).",
            req.draft.word_count
        ));
    }
    if !req.draft.body_md.contains("Thank") && !req.draft.body_md.contains("Thanks") {
        suggestions.push("Consider closing with a courteous thank-you.".into());
    }

    let summary = if ok {
        "Draft looks good."
    } else {
        "Draft needs attention."
    };

    let notes = ReviewNotes {
        summary: summary.into(),
        suggestions,
    };

    Ok(Review { ok, notes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use runloop_agents_common::DraftArtifact;
    use runloop_core::ids::EventId;
    use std::path::PathBuf;

    fn draft_with(body: &str, word_count: usize) -> DraftArtifact {
        DraftArtifact {
            artifact_id: EventId(1),
            path: PathBuf::from("/tmp/draft.md"),
            sha256: "abc123".into(),
            body_md: body.into(),
            rationale: "test".into(),
            citations: Vec::new(),
            word_count,
        }
    }

    #[tokio::test]
    async fn flags_too_long_drafts() {
        let body = "word ".repeat(190);
        let review = critique(ReviewRequest {
            draft: draft_with(&body, 190),
        })
        .await
        .expect("review succeeds");
        assert!(!review.ok);
        assert!(
            review
                .notes
                .suggestions
                .iter()
                .any(|s| s.contains("Trim the draft"))
        );
    }

    #[tokio::test]
    async fn suggests_thank_you_when_missing() {
        let review = critique(ReviewRequest {
            draft: draft_with("Hello there", 2),
        })
        .await
        .expect("review succeeds");
        assert!(
            review
                .notes
                .suggestions
                .iter()
                .any(|s| s.contains("thank-you"))
        );
    }
}
