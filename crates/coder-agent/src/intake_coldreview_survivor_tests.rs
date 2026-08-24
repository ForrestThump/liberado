//! Split from `intake_session.rs` and `cold_review.rs`: kills the baseline
//! campaign's survivors in both files.
//!
//! intake: the system prompt actually sent, context/answer section gating.
//! cold review: empty-blob isolation guard, excerpt-path membership, and the
//! post-fix-round decision table.

use super::{IntakeAnswer, run_intake};
use crate::cold_review::{
    ChangeSurface, ForbiddenAuthorContext, MAX_FIX_ROUNDS, StageDecision,
    build_cold_review_request, decide_after_fix_round,
};
use liberado_provider::{CompletionRequest, CompletionResponse, MockProvider};

fn surface() -> ChangeSurface {
    ChangeSurface {
        diff: "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-old\n+new\n".into(),
        file_excerpts: Vec::new(),
    }
}

fn forbidden() -> ForbiddenAuthorContext {
    ForbiddenAuthorContext {
        goal_narrative: None,
        tool_trace: None,
        prior_agent_chat: None,
    }
}

#[test]
fn a_blank_forbidden_blob_does_not_trip_the_isolation_guard() {
    // `contains("")` is always true; only non-empty blobs may be searched for.
    let f = ForbiddenAuthorContext {
        goal_narrative: Some("   ".into()),
        ..forbidden()
    };
    let result = build_cold_review_request(&surface(), &f, None, "/ws");
    assert!(result.is_ok(), "{result:?}");
}

/// A non-empty author blob is refused outright, before any prompt assembly —
/// the reviewer never sees it at all, in any shape.
#[test]
fn any_real_author_blob_is_refused_upfront() {
    let f = ForbiddenAuthorContext {
        goal_narrative: Some("secret acceptance wording".into()),
        ..forbidden()
    };
    let result = build_cold_review_request(&surface(), &f, None, "/ws");
    assert!(
        matches!(&result, Err(msg) if msg.contains("must not receive goal_narrative")),
        "{result:?}"
    );
}

#[test]
fn an_excerpt_inside_the_change_is_accepted() {
    let s = ChangeSurface {
        file_excerpts: vec![("src/main.rs".into(), "the new body".into())],
        ..surface()
    };
    assert!(build_cold_review_request(&s, &forbidden(), None, "/ws").is_ok());
}

#[test]
fn an_excerpt_outside_the_change_is_rejected() {
    let s = ChangeSurface {
        file_excerpts: vec![("src/other.rs".into(), "not part of this change".into())],
        ..surface()
    };
    let result = build_cold_review_request(&s, &forbidden(), None, "/ws");
    assert!(
        matches!(&result, Err(msg) if msg.contains("outside the change surface")),
        "{result:?}"
    );
}

/// complete_json retries once without a schema when the backend refuses the
/// schema, so the mock needs a spare answer to stay alive through both calls.
fn scripted_intake(response: &str) -> MockProvider {
    let provider = MockProvider::new("mock");
    provider.push(CompletionResponse::text(response));
    provider.push(CompletionResponse::text(response));
    provider
}

async fn last_user_message(provider: &MockProvider) -> String {
    let requests: Vec<CompletionRequest> = provider.received_requests();
    let last = requests.last().expect("a request was made");
    last.messages
        .iter()
        .find(|m| m.role == liberado_provider::Role::User)
        .map(|m| m.content.clone())
        .expect("user message")
}

const CLARIFY_JSON: &str = r#"{"status":"needs_clarification","questions":[{"id":"q1","prompt":"Which crate?"}],"partial_draft":null}"#;

#[tokio::test]
async fn intake_sends_the_built_in_system_prompt() {
    let provider = scripted_intake(CLARIFY_JSON);
    // Prompt-shape assertions only need the request; the mocked reply may or may
    // not decode, so the result is deliberately ignored.
    let _ = run_intake(&provider, "build a cli", &[], None).await;
    let requests = provider.received_requests();
    let system = requests[0]
        .messages
        .iter()
        .find(|m| m.role == liberado_provider::Role::System)
        .map(|m| m.content.clone())
        .expect("system message");
    assert_eq!(system, liberado_coder_core::prompts::INTAKE);
}

#[tokio::test]
async fn blank_context_adds_no_context_section() {
    let provider = scripted_intake(CLARIFY_JSON);
    let _ = run_intake(&provider, "build a cli", &[], Some("   \n\t")).await;
    let user = last_user_message(&provider).await;
    assert!(!user.contains("Additional context"), "{user}");
}

#[tokio::test]
async fn real_context_is_included_and_trimmed() {
    let provider = scripted_intake(CLARIFY_JSON);
    let _ = run_intake(
        &provider,
        "  build a cli  ",
        &[],
        Some("  the vault layout \n"),
    )
    .await;
    let user = last_user_message(&provider).await;
    assert!(user.contains("Human goal writeup:\nbuild a cli"), "{user}");
    assert!(user.contains("Additional context"), "{user}");
    assert!(user.contains("the vault layout"), "{user}");
}

#[tokio::test]
async fn no_answers_means_no_answers_section() {
    let provider = scripted_intake(CLARIFY_JSON);
    let _ = run_intake(&provider, "build a cli", &[], None).await;
    let user = last_user_message(&provider).await;
    assert!(!user.contains("Human answers"), "{user}");
}

#[tokio::test]
async fn prior_answers_are_listed() {
    let provider = scripted_intake(CLARIFY_JSON);
    let answers = vec![IntakeAnswer {
        question_id: "q1".into(),
        answer: "the server crate".into(),
    }];
    let _ = run_intake(&provider, "build a cli", &answers, None).await;
    let user = last_user_message(&provider).await;
    assert!(user.contains("Human answers to prior questions:"), "{user}");
    assert!(user.contains("- q1: the server crate"), "{user}");
}

#[test]
fn no_fix_needed_only_when_verify_passed_and_nothing_retained() {
    // A failed re-verify with zero retained findings is NOT success: it escalates
    // once the fix budget is gone instead of claiming the work is done.
    assert!(matches!(
        decide_after_fix_round(MAX_FIX_ROUNDS, false, 0),
        StageDecision::EscalateToHuman { .. }
    ));
}

#[test]
fn a_real_fix_round_never_reads_as_pre_round() {
    // rounds == 0 is the misuse signal; one completed round must take the normal path.
    match decide_after_fix_round(MAX_FIX_ROUNDS, false, 5) {
        StageDecision::EscalateToHuman { reason } => {
            assert!(
                reason.contains("fix round(s)"),
                "escalation must be about the budget, not misuse: {reason}"
            );
        }
        other => panic!("expected escalation, got {other:?}"),
    }
}

#[test]
fn zero_rounds_is_the_misuse_signal() {
    match decide_after_fix_round(0, true, 0) {
        StageDecision::EscalateToHuman { reason } => {
            assert!(reason.contains("before a fix round"), "{reason}");
        }
        other => panic!("zero rounds must refuse to decide, got {other:?}"),
    }
}

/// A green re-verify means nothing while findings are still retained — that is
/// "keep fixing", not success.
#[test]
fn passed_reverify_with_retained_findings_is_not_success() {
    let decision = decide_after_fix_round(MAX_FIX_ROUNDS, true, 3);
    assert!(
        !matches!(decision, StageDecision::NoFixNeeded),
        "retained findings block a clean verdict: {decision:?}"
    );
}
