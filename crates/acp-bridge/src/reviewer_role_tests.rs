//! Split from `coding_run.rs` for module-health boundaries.

use super::*;
use liberado_coder_agent::assemble::entry;
use liberado_provider::{CompletionResponse, Message, MockProvider};

fn tuning_with_critic(model: &str) -> CoderTuning {
    CoderTuning {
        critic: CoderRoleConfig {
            model: model.to_string(),
            prompt_path: None,
            prompt: Some("placeholder".into()),
            temperature: None,
            max_tokens: None,
            max_turns: Some(1),
            reasoning: None,
        },
        ..CoderTuning::default()
    }
}

/// Critic role resolved through the shared ACP assembly path (not a local copy).
fn acp_critic_role(critic_model: &str, session_model: &str) -> CoderRoleConfig {
    let tuning = tuning_with_critic(critic_model);
    let assembled = assemble_production_run(
        &tuning,
        entry::acp_surface(
            CoderTask::new("t", "goal"),
            PathBuf::from("."),
            Some(session_model.into()),
            Some(10),
            0,
            Vec::new(),
        ),
    );
    assembled.request.config.critic
}

/// The reviewer must run on the model `[coder.critic]` names, not the coder's.
///
/// It took the session model, so both reviewers ran on `deepseek-v4-pro` while the config
/// said `deepseek-v4-flash`. Reviewing a diff is a cheaper job than writing one, and paying
/// the difference silently is exactly the shape of the other shadowed settings.
#[test]
fn the_reviewer_uses_the_configured_model_not_the_coders() {
    let role = acp_critic_role("deepseek-v4-flash", "deepseek-v4-pro");
    assert_eq!(
        role.model, "deepseek-v4-flash",
        "the configured critic model must win over the session's"
    );
}

/// The trace is only useful when its role model is the id on the wire. ACP used one mutable
/// provider for the whole run, so the critic label said flash while the request still named
/// the session's pro model.
#[tokio::test]
async fn the_role_provider_sends_the_configured_model() {
    let inner = Arc::new(MockProvider::with_script(
        "session-model",
        [CompletionResponse::text("ok")],
    ));
    let factory = role_factory(Arc::clone(&inner) as Arc<dyn Provider>);
    let critic = tuning_with_critic("critic-model").critic;
    let provider = factory.provider_for("critic", &critic).unwrap();

    provider
        .complete(CompletionRequest::new(vec![Message::user("review")]))
        .await
        .unwrap();

    assert_eq!(provider.model(), "critic-model");
    assert_eq!(
        inner.last_request().and_then(|request| request.model),
        Some("critic-model".to_string()),
        "the configured role model must override the session provider model on the wire"
    );
}

#[tokio::test]
async fn the_role_provider_sends_configured_reasoning_effort() {
    let inner = Arc::new(MockProvider::with_script(
        "session-model",
        [CompletionResponse::text("ok")],
    ));
    let factory = role_factory(Arc::clone(&inner) as Arc<dyn Provider>);
    let mut critic = tuning_with_critic("critic-model").critic;
    critic.reasoning = Some("high".into());
    let provider = factory.provider_for("critic", &critic).unwrap();

    provider
        .complete(CompletionRequest::new(vec![Message::user("review")]))
        .await
        .unwrap();

    assert_eq!(
        inner.last_request().and_then(|request| request.reasoning),
        Some("high".to_string()),
        "ACP coding construction must put role reasoning on the outbound request"
    );
}

/// With no critic model configured, fall back to the session's rather than dispatching to an
/// empty model id, which fails at the provider with a worse message.
#[test]
fn an_unset_critic_model_falls_back_to_the_session_model() {
    let role = acp_critic_role("  ", "deepseek-v4-pro");
    assert_eq!(role.model, "deepseek-v4-pro");
}

/// An open finding must sit above the workspace path and the file list.
///
/// This is the entire "do not bury the finding" mechanism, and it is one `push_str` away from
/// silently reverting to a footnote under a trace path nobody scrolls to.
#[test]
fn findings_are_rendered_above_the_housekeeping() {
    let outcome = CodingRoundOutcome {
        summary: "did the thing".into(),
        outcome: "Succeeded".into(),
        files_changed: vec!["src/main.rs".into()],
        workspace: "/tmp/ws".into(),
        trace_path: Some("/tmp/trace.json".into()),
        validation_notes: None,
        findings: "## Review findings

- the test does not bind
"
        .into(),
    };
    let rendered = outcome.render();
    let finding_at = rendered
        .find("the test does not bind")
        .expect("finding shown");
    let workspace_at = rendered.find("**Workspace:**").expect("workspace shown");
    assert!(
        finding_at < workspace_at,
        "an open finding must not sit below the housekeeping:
{rendered}"
    );
}

/// No findings must render exactly as before — no stray heading, no blank section.
#[test]
fn a_clean_round_renders_no_findings_section() {
    let outcome = CodingRoundOutcome {
        summary: "did the thing".into(),
        outcome: "Succeeded".into(),
        files_changed: Vec::new(),
        workspace: "/tmp/ws".into(),
        trace_path: None,
        validation_notes: None,
        findings: String::new(),
    };
    assert!(!outcome.render().contains("Review findings"));
}

/// The completion gate must be *usable* when it is switched on, not merely reachable.
///
/// `[coder.gate] enabled = true` parses, reaches `run_gate`, and asks `role_instructions` for
/// the reviewer's prompt. That call returns `Err` for a role with neither `prompt` nor
/// `prompt_path`, and `run_attempt` propagates it — so with a promptless role here, turning the
/// gate on failed the whole run at the first reviewer. Reverting the shared assembler to a
/// promptless critic must fail this test rather than wait to be discovered by a user who
/// enabled a setting.
#[test]
fn the_gate_reviewer_role_can_actually_be_instructed() {
    let role = acp_critic_role("cfg/model", "session/model");
    let prompt = role
        .prompt
        .as_deref()
        .or(role.prompt_path.as_deref())
        .unwrap_or("");
    assert!(
        !prompt.trim().is_empty(),
        "a reviewer role with no prompt makes `[coder.gate] enabled = true` fail the run"
    );
    assert!(
        !role.model.trim().is_empty(),
        "a reviewer with no model cannot be dispatched to a provider"
    );
}

/// The prompt must be the shared one, not a copy. A copy drifts from whatever gets measured
/// offline, and then the score describes a reviewer that never runs.
#[test]
fn the_reviewer_uses_the_shared_prompt() {
    assert_eq!(
        acp_critic_role("m", "m").prompt.as_deref(),
        Some(liberado_coder_agent::COLD_DIFF_REVIEWER_PROMPT),
    );
}
