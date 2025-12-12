use runloop_agents_common::{AgentResult, DraftArtifact, Review, ReviewNotes};
use serde::{Deserialize, Serialize};

const MIN_WORDS: usize = 10;
const MAX_WORDS: usize = 180;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub draft: DraftArtifact,
}

pub async fn critique(req: ReviewRequest) -> AgentResult<Review> {
    let mut suggestions = Vec::new();
    let mut blocking = false;

    let word_count = effective_word_count(&req.draft.body_md, req.draft.word_count);

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
    if !has_thank_you(&req.draft.body_md) {
        suggestions.push("Consider closing with a courteous thank-you.".into());
    }

    let ok = !blocking;
    let summary = match (ok, suggestions.is_empty()) {
        (true, true) => "Draft looks good.",
        (true, false) => "Draft looks good with minor polish.",
        (false, _) => "Draft needs attention.",
    };

    let notes = ReviewNotes {
        summary: summary.into(),
        suggestions,
    };

    Ok(Review { ok, notes })
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
    use runloop_agents_common::DraftArtifact;
    use runloop_core::ids::EventId;
    use std::path::PathBuf;

    fn draft_with(body: &str) -> DraftArtifact {
        DraftArtifact {
            artifact_id: EventId(1),
            path: PathBuf::from("/tmp/draft.md"),
            sha256: "abc123".into(),
            body_md: body.into(),
            rationale: "test".into(),
            citations: Vec::new(),
            word_count: body.split_whitespace().count(),
        }
    }

    #[tokio::test]
    async fn flags_too_long_drafts() {
        let body = "word ".repeat(190);
        let review = critique(ReviewRequest {
            draft: draft_with(&body),
        })
        .await
        .expect("review succeeds");
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

    #[tokio::test]
    async fn blocks_too_short_draft() {
        let review = critique(ReviewRequest {
            draft: draft_with("Hello there"),
        })
        .await
        .expect("review succeeds");
        assert!(!review.ok, "short drafts should be blocking");
        assert_eq!(review.notes.summary, "Draft needs attention.");
        assert!(
            review
                .notes
                .suggestions
                .iter()
                .any(|s| s.contains("at least"))
        );
    }

    #[tokio::test]
    async fn suggests_thank_you_when_missing() {
        let body = "hello ".repeat(25);
        let review = critique(ReviewRequest {
            draft: draft_with(&body),
        })
        .await
        .expect("review succeeds");
        assert!(review.ok, "missing thank-you is advisory, not blocking");
        assert_eq!(review.notes.summary, "Draft looks good with minor polish.");
        assert!(
            review
                .notes
                .suggestions
                .iter()
                .any(|s| s.contains("thank-you"))
        );
    }

    #[tokio::test]
    async fn passes_well_formed_draft() {
        let body = "thanks for reviewing this draft. it covers the requested updates and next steps for the team."
            .repeat(2);
        let review = critique(ReviewRequest {
            draft: draft_with(&body),
        })
        .await
        .expect("review succeeds");
        assert!(review.ok);
        assert_eq!(review.notes.summary, "Draft looks good.");
        assert!(review.notes.suggestions.is_empty());
    }

    #[tokio::test]
    async fn thank_you_check_is_case_insensitive() {
        let body = "thanks for your help pulling the data together for this release. the details were really useful for closing out the plan.";
        let review = critique(ReviewRequest {
            draft: draft_with(body),
        })
        .await
        .expect("review succeeds");
        assert!(review.ok);
        assert_eq!(review.notes.summary, "Draft looks good.");
        assert!(review.notes.suggestions.is_empty());
    }
}
