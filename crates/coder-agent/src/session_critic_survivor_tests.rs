//! Split from `session_critic.rs`: kills the baseline campaign's survivors.
//!
//! Covers transcript rendering (text turns, call attribution, silent-turn
//! dropping), the system instructions actually sent, the empty-transcript
//! guard's conjunction, review parsing, and the clean-review shape.

use super::*;
use liberado_provider::{CompletionResponse, Provider};
use std::sync::Arc;

struct FixedProvider(Option<Arc<dyn Provider>>);

impl crate::CoderProviderFactory for FixedProvider {
    fn provider_for(
        &self,
        _role: &str,
        _config: &CoderRoleConfig,
    ) -> Result<Arc<dyn Provider>, CoderError> {
        match &self.0 {
            Some(p) => Ok(Arc::clone(p)),
            None => Err(CoderError::Setup("no session-critic provider".into())),
        }
    }
}

/// Returns the trait object for wiring plus the concrete handle for assertions.
fn critic_provider(reply: &str) -> (Arc<dyn Provider>, Arc<liberado_provider::MockProvider>) {
    let provider = liberado_provider::MockProvider::new("mock");
    provider.push(CompletionResponse::text(reply));
    let typed = Arc::new(provider);
    (typed.clone(), typed)
}

fn gate_request() -> CoderRunRequest {
    serde_json::from_value(serde_json::json!({
        "task": {"id": "t", "description": "add --version"},
        "workspace": {"root": "/tmp/ws", "base_ref": "main"},
        "config": {
            "backend": "loop",
            "planner": {"model": "p"},
            "coder": {"model": "c"},
            "critic": {"model": "cr"}
        }
    }))
    .expect("request json")
}

fn turn(turn: u32, content: Option<&str>) -> CoderEvent {
    CoderEvent::ModelTurnFinished {
        role: "worker".into(),
        turn,
        tools_offered: Vec::new(),
        message_count: 1,
        content: content.map(str::to_string),
        finish_reason: "prose".into(),
        tool_calls: Vec::new(),
        prompt_tokens: 0,
        completion_tokens: 0,
        at: chrono::Utc::now(),
    }
}

fn tool_started(name: &str) -> CoderEvent {
    CoderEvent::ToolStarted {
        name: name.into(),
        args_preview: "{}".into(),
        at: chrono::Utc::now(),
    }
}

#[test]
fn the_transcript_carries_text_and_attributes_calls() {
    let events = vec![
        turn(1, Some("I will write the file now")),
        tool_started("write_file"),
        turn(2, None), // carries the pending call
        turn(3, None), // neither text nor calls: dropped
    ];
    let transcript = build_transcript(&events, ToolVisibility::NamesOnly);
    assert!(
        transcript.contains("I will write the file now"),
        "{transcript}"
    );
    assert!(transcript.contains("--- turn 2 ---"), "{transcript}");
    assert!(
        transcript.contains("[called: write_file({})]"),
        "{transcript}"
    );
    assert!(
        !transcript.contains("turn 3"),
        "a turn with neither text nor calls is dropped: {transcript}"
    );
}

#[test]
fn text_only_visibility_hides_call_names() {
    let events = vec![tool_started("write_file"), turn(1, Some("writing"))];
    let transcript = build_transcript(&events, ToolVisibility::TextOnly);
    assert!(!transcript.contains("[called:"), "{transcript}");
}

/// A spoken transcript alone must reach the reviewer even with no filed report:
/// the nothing-to-audit shortcut needs BOTH sides empty.
#[tokio::test]
async fn a_transcript_without_a_report_still_reviews() {
    // No provider configured: reaching provider_for proves the shortcut did not fire.
    let providers = FixedProvider(None);
    let result = review_session(
        &providers,
        &gate_request(),
        &CoderRoleConfig::default(),
        &[turn(1, Some("spoken"))],
        None,
        ToolVisibility::NamesOnly,
    )
    .await;
    assert!(
        result.is_err(),
        "must attempt the reviewer call, not default"
    );
}

#[tokio::test]
async fn an_empty_transcript_with_a_filed_report_still_reviews() {
    let providers = FixedProvider(None);
    let result = review_session(
        &providers,
        &gate_request(),
        &CoderRoleConfig::default(),
        &[],
        Some("the operator was told it worked"),
        ToolVisibility::NamesOnly,
    )
    .await;
    assert!(result.is_err(), "a filed report is evidence too");
}

#[tokio::test]
async fn the_reviewer_receives_the_built_in_instructions() {
    let (provider, mock) = critic_provider(r#"{"findings":[]}"#);
    let providers = FixedProvider(Some(provider));
    let _ = review_session(
        &providers,
        &gate_request(),
        &CoderRoleConfig::default(),
        &[turn(1, Some("did the thing"))],
        None,
        ToolVisibility::NamesOnly,
    )
    .await;
    let requests = mock.received_requests();
    let system = requests[0]
        .messages
        .iter()
        .find(|m| m.role == liberado_provider::Role::System)
        .map(|m| m.content.clone())
        .expect("system message");
    assert_eq!(system, liberado_coder_core::prompts::SESSION_CRITIC);
}

#[tokio::test]
async fn findings_survive_the_round_trip() {
    let reply = r#"{"findings":[{"kind":"unsupported_claim","quote":"all tests pass","why":"no test ran in the trace"}]}"#;
    let providers = FixedProvider(Some(critic_provider(reply).0));
    let review = review_session(
        &providers,
        &gate_request(),
        &CoderRoleConfig::default(),
        &[turn(1, Some("all tests pass"))],
        None,
        ToolVisibility::NamesOnly,
    )
    .await
    .expect("parse");
    assert_eq!(review.findings.len(), 1, "{review:?}");
    assert_eq!(review.findings[0].kind, "unsupported_claim");
    assert_eq!(review.findings[0].quote, "all tests pass");
}

#[tokio::test]
async fn a_nonempty_transcript_alone_reaches_the_reviewer() {
    let providers = FixedProvider(Some(critic_provider(r#"{"findings":[]}"#).0));
    let review = review_session(
        &providers,
        &gate_request(),
        &CoderRoleConfig::default(),
        &[turn(1, Some("spoken"))],
        None,
        ToolVisibility::NamesOnly,
    )
    .await
    .expect("a spoken turn is enough to review");
    assert!(review.is_clean());
}

#[test]
fn prose_without_an_object_is_an_error_not_a_clean_review() {
    let err = parse_session_review("the run looked fine to me").unwrap_err();
    assert!(err.to_string().contains("no JSON object"), "{err}");
}

#[test]
fn a_lone_open_brace_is_no_object() {
    let err = parse_session_review("{").unwrap_err();
    assert!(
        err.to_string().contains("no JSON object"),
        "an empty span must take the not-found path: {err}"
    );
}

#[test]
fn a_closing_brace_before_the_object_takes_the_not_found_path() {
    // First '{' sits AFTER the last '}': the span would be inverted.
    let err = parse_session_review("prose } more { tail").unwrap_err();
    assert!(err.to_string().contains("no JSON object"), "{err}");
}

#[test]
fn a_brace_before_the_object_is_still_found() {
    // The closing brace of a fence-quote before the real object: the span
    // between first '{' and last '}' is what gets parsed.
    let review = parse_session_review(
        "noise } then {\"findings\":[{\"kind\":\"k\",\"quote\":\"q\",\"why\":\"w\"}]}",
    )
    .expect("object found");
    assert_eq!(review.findings.len(), 1);
}
