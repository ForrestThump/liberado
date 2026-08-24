//! Split from `lib.rs`: kills the baseline campaign's survivors.
//!
//! Direct units (backend name, retryable-error table, trace reading, diff
//! assembly boundaries, checkpoint emission, judgment gating) plus end-to-end
//! attempt-loop runs that pin the revision-retry and strategist machinery.

use super::*;
use liberado_coder_core::{
    CoderRoleConfig, CoderRunConfig, CoderTask, CoderTrace, CommandPolicy, PathPolicy,
    ProgressPolicy, SandboxSpec, VerdictStatus, WorkspaceRef,
};
use liberado_coder_sandbox::failure_identities;
use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};
use liberado_session::SessionEventKind;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;

// ── shared fixtures ─────────────────────────────────────────────────────────

fn init_repo(root: &Path) {
    let run = |args: &[&str]| {
        let out = liberado_common::process::std_command("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git available");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init", "--quiet"]);
    run(&["config", "user.email", "test@liberado.local"]);
    run(&["config", "user.name", "Test"]);
    run(&["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("tracked.rs"), "fn old() {}\n").unwrap();
    run(&["add", "tracked.rs"]);
    run(&["commit", "--quiet", "-m", "base"]);
}

fn base_request(root: &Path) -> CoderRunRequest {
    let worker_role = CoderRoleConfig {
        max_turns: Some(12),
        ..CoderRoleConfig::default()
    };
    let critic_role = CoderRoleConfig {
        max_turns: Some(12),
        prompt: Some("Review the change strictly.".into()),
        ..CoderRoleConfig::default()
    };
    CoderRunRequest {
        task: CoderTask::new("task-1", "write hello.txt"),
        workspace: WorkspaceRef::new(root.to_string_lossy(), "HEAD"),
        config: CoderRunConfig {
            backend: LIBERADO_LOOP_BACKEND.to_string(),
            trace_dir: None,
            trace_formats: Vec::new(),
            planner: CoderRoleConfig::default(),
            coder: worker_role.clone(),
            critic: critic_role.clone(),
            gate: liberado_coder_core::CoderGateConfig::default(),
            repair: None,
            sandbox: SandboxSpec::HostLocal,
            command_policy: CommandPolicy::default(),
            validation_command: None,
            verifiers: Vec::new(),
            verify_policy: Default::default(),
            path_policy: PathPolicy::default(),
            progress: ProgressPolicy::default(),
            hashline: Default::default(),
            session_critic: Default::default(),
            prompt_dir: None,
            edit: Default::default(),
            workspace_build: Default::default(),
            offered_tools: None,
        },
        attempt: 0,
        prior_feedback: Vec::new(),
        strategist_directive: None,
    }
}

fn write_then_report() -> [CompletionResponse; 2] {
    [
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "write-1",
            "write_file",
            json!({"path": "hello.txt", "content": "hello\n"}),
        )]),
        CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "report-1",
            liberado_executor::SUBMIT_REPORT_TOOL,
            json!({
                "outcome": "succeeded",
                "summary": "did the thing",
                "artifacts": [],
                "new_high_signal_facts": [],
                "follow_up": null
            }),
        )]),
    ]
}

fn critic_reply(quality: &str, issues: &[&str]) -> CompletionResponse {
    CompletionResponse::text(
        json!({
            "quality": quality,
            "issues": issues
        })
        .to_string(),
    )
}

// ── direct units ────────────────────────────────────────────────────────────

#[test]
fn the_backend_is_named_after_its_constant() {
    let provider = Arc::new(MockProvider::new("mock"));
    assert_eq!(
        LiberadoLoopBackend::new(provider).name(),
        LIBERADO_LOOP_BACKEND
    );
    assert!(!LIBERADO_LOOP_BACKEND.is_empty());
}

#[test]
fn a_trace_that_already_recorded_its_ending_is_not_re_stamped() {
    let log: trace::EventLog = Arc::new(std::sync::Mutex::new(Vec::new()));
    assert!(!ended_in_trace(&log), "an empty log has no ending");
    trace::push_event(
        &log,
        CoderEvent::SessionFinished {
            outcome: Outcome::Succeeded,
            at: Utc::now(),
        },
    );
    assert!(ended_in_trace(&log));
}

#[tokio::test]
async fn trace_reading_round_trips_events_and_fails_soft() {
    let dir = tempfile::tempdir().unwrap();
    let request = base_request(dir.path());
    let events = vec![CoderEvent::ModelTurnFinished {
        role: "worker".into(),
        turn: 1,
        tools_offered: vec![],
        message_count: 1,
        content: Some("spoken".into()),
        finish_reason: "prose".into(),
        tool_calls: vec![],
        prompt_tokens: 0,
        completion_tokens: 0,
        at: Utc::now(),
    }];
    let path = dir.path().join("t.json");
    let doc = CoderTrace {
        session_id: "s".into(),
        request,
        events: events.clone(),
        result: None,
    };
    std::fs::write(&path, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
    let read = LiberadoLoopBackend::read_trace_events(&path.to_string_lossy())
        .await
        .expect("a valid trace reads back");
    assert_eq!(read.len(), 1, "the events come back, not an empty list");

    let garbage = dir.path().join("bad.json");
    std::fs::write(&garbage, "not json").unwrap();
    assert!(
        LiberadoLoopBackend::read_trace_events(&garbage.to_string_lossy())
            .await
            .is_none()
    );
    assert!(
        LiberadoLoopBackend::read_trace_events(&dir.path().join("absent.json").to_string_lossy())
            .await
            .is_none()
    );
}

/// With no tracked changes the untracked section starts the diff directly; with
/// tracked edits there is exactly one blank line before it — never glued on.
#[tokio::test]
async fn the_untracked_section_is_separated_from_the_tracked_diff() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("tracked.rs"), "fn changed() {}\n").unwrap();
    std::fs::write(dir.path().join("added.rs"), "fn brand_new() {}\n").unwrap();
    let diff = workspace_diff(&dir.path().to_string_lossy()).await.unwrap();
    // The pushed separator plus git's own trailing newline gives one blank line.
    assert!(diff.contains("\n\n# untracked files"), "{diff}");

    let clean = tempfile::tempdir().unwrap();
    init_repo(clean.path());
    std::fs::write(clean.path().join("added.rs"), "fn brand_new() {}\n").unwrap();
    let diff2 = workspace_diff(&clean.path().to_string_lossy())
        .await
        .unwrap();
    assert!(
        diff2.starts_with("# untracked files"),
        "an untracked-only diff has no leading blank: {diff2:?}"
    );
}

/// A directory that is not a git repo makes `git diff` fail; that must surface
/// as an error, not as an empty diff wearing a success shape.
#[tokio::test]
async fn a_non_repo_workspace_is_a_diff_error() {
    let dir = tempfile::tempdir().unwrap();
    let err = workspace_diff(&dir.path().to_string_lossy())
        .await
        .expect_err("no repo, no diff");
    assert!(err.to_string().contains("git diff"), "{err}");
}

#[tokio::test]
async fn truncation_marks_only_what_was_cut() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let body = "line\n".repeat(40_000); // well past the untracked budget
    std::fs::write(dir.path().join("big.txt"), &body).unwrap();

    let diff = workspace_diff(&dir.path().to_string_lossy()).await.unwrap();
    assert!(diff.contains("… truncated"), "{diff}");
    assert!(
        !diff.contains("\n\n… truncated"),
        "the body already ends in a newline; the marker must not add another: {diff}"
    );

    // An exact fit is not a truncation.
    let dir2 = tempfile::tempdir().unwrap();
    init_repo(dir2.path());
    let exact = "x".repeat(1_000);
    std::fs::write(dir2.path().join("exact.txt"), &exact).unwrap();
    let diff2 = workspace_diff(&dir2.path().to_string_lossy())
        .await
        .unwrap();
    assert!(
        !diff2.contains("truncated"),
        "content within budget is shown in full: {diff2}"
    );
}

#[tokio::test]
async fn fully_shown_content_without_a_trailing_newline_gets_one() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("noeol.txt"), "no trailing newline").unwrap();
    let diff = workspace_diff(&dir.path().to_string_lossy()).await.unwrap();
    assert!(
        diff.ends_with("no trailing newline\n"),
        "the section terminator must not glue onto content: {diff:?}"
    );
}

#[test]
fn a_latched_fatal_is_consumed_once_the_report_is_filed() {
    let policy = ProgressPolicy {
        read_only_turn_limit: 2,
        same_tool_limit: 2,
        validation_repeat_limit: 2,
        max_attempts: 3,
        event_preview_max_chars: 100,
    };
    let mut guard = ProgressGuard::new(policy);
    for t in ["list_files", "read_file", "git_status", "search_text"] {
        guard.observe(t, true, "{}");
    }
    let shared = Arc::new(std::sync::Mutex::new(guard));
    log_ignored_fatal(&shared, Outcome::Succeeded);
    assert!(
        shared.lock().unwrap().take_fatal().is_none(),
        "the ignored fatal is logged AND cleared, not left latched"
    );
}

#[tokio::test]
async fn a_checkpoint_event_reaches_the_live_bus() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    crate::live::with_live_events(tx, "s1", async {
        take_workspace_checkpoint(dir.path(), "s1", "before-edit").await;
    })
    .await;
    let event = rx.try_recv().expect("checkpoint emitted");
    match event.kind {
        SessionEventKind::Checkpoint { label, .. } => assert_eq!(label, "before-edit"),
        other => panic!("expected Checkpoint, got {other:?}"),
    }
}

/// The soften step must also CLEAR the failure evidence when it upgrades the
/// verdict — a pass that still carries findings is not a pass.
#[test]
fn softening_a_pre_existing_failure_clears_the_findings() {
    use liberado_coder_core::{Finding, FindingKind, NamedVerdict, Verdict};
    let log = "failures:\n    test wire::tests::case_one ... FAILED\n";
    let pipeline = liberado_coder_core::PipelineResult {
        overall: VerdictStatus::Fail,
        results: vec![NamedVerdict {
            id: "cargo-test".into(),
            kind: "command".into(),
            verdict: Verdict::fail(
                "1 failing",
                vec![Finding {
                    check_id: "cargo-test".into(),
                    kind: FindingKind::CommandFailed,
                    message: "test failed".into(),
                    detail: None,
                }],
                Some(log.to_string()),
            ),
        }],
        combined_findings: vec![],
        combined_signature: Some("sig".into()),
    };
    let baseline: std::collections::BTreeSet<String> =
        failure_identities(log).into_iter().collect();
    assert!(!baseline.is_empty(), "premise: the failure is identifiable");
    let softened = soften_pre_existing_test_failures(&pipeline, &baseline);
    assert_eq!(softened.overall, VerdictStatus::Pass, "{softened:?}");
    assert!(
        softened.combined_findings.is_empty(),
        "a softened pass must not carry failure findings: {softened:?}"
    );
    assert!(softened.combined_signature.is_none());
}

/// An empty change with the gate enabled must not reach the reviewers at all:
/// there is no claim to dispute, and the provider must not be asked.
#[tokio::test]
async fn an_unchanged_workspace_skips_the_gate_entirely() {
    let provider = Arc::new(MockProvider::new("mock"));
    let providers: Arc<dyn CoderProviderFactory> =
        Arc::new(SingleProviderFactory::new(provider.clone()));
    let dir = tempfile::tempdir().unwrap();
    let mut request = base_request(dir.path());
    request.config.gate.enabled = true;
    let mut state = VerdictState {
        outcome: Outcome::Succeeded,
        summary: "clean".into(),
        critic_verdict: None,
        gate_votes: vec![],
    };
    apply_judgment(
        providers.as_ref(),
        &request,
        &[],
        &[], // nothing changed
        &Arc::new(std::sync::Mutex::new(Vec::new())),
        &mut state,
    )
    .await
    .expect("no reviewers, no error");
    assert_eq!(
        provider.received_requests().len(),
        0,
        "an empty diff must not spend reviewer calls"
    );
}

// ── session critic wiring ───────────────────────────────────────────────────

fn critic_config(enabled: bool) -> liberado_coder_core::CoderRunConfig {
    let mut config = base_request(Path::new("/tmp")).config;
    config.session_critic.enabled = enabled;
    config
}

fn finding_review_reply() -> CompletionResponse {
    CompletionResponse::text(
        json!({"findings":[{"kind":"unsupported_claim","quote":"all tests pass","why":"none ran"}]})
            .to_string(),
    )
}

/// Drives the post-run hook through a REAL trace file on disk, so both the
/// enabled and disabled paths are exercised end to end.
async fn run_review_after(
    config: liberado_coder_core::CoderRunConfig,
    reply: CompletionResponse,
) -> (CoderRunResult, usize) {
    let dir = tempfile::tempdir().unwrap();
    let request = base_request(dir.path());
    let events = vec![CoderEvent::ModelTurnFinished {
        role: "worker".into(),
        turn: 1,
        tools_offered: vec![],
        message_count: 1,
        content: Some("I wrote the file and all tests pass".into()),
        finish_reason: "prose".into(),
        tool_calls: vec![],
        prompt_tokens: 0,
        completion_tokens: 0,
        at: Utc::now(),
    }];
    let trace_path = dir.path().join("attempt.json");
    let doc = CoderTrace {
        session_id: "s".into(),
        request: request.clone(),
        events,
        result: None,
    };
    std::fs::write(&trace_path, serde_json::to_vec(&doc).unwrap()).unwrap();

    let provider = Arc::new(MockProvider::with_script("mock", [reply]));
    let received = provider.received_requests().len();
    let backend = LiberadoLoopBackend::new(provider.clone());
    let mut result = CoderRunResult {
        backend: LIBERADO_LOOP_BACKEND.into(),
        outcome: Outcome::Succeeded,
        summary: "all tests pass".to_string(),
        files_changed: vec![],
        file_changes: Vec::new(),
        validation_notes: None,
        critic_verdict: None,
        gate_votes: vec![],
        trace_path: Some(trace_path.to_string_lossy().into_owned()),
        diff_findings: vec![],
        session_findings: vec![],
        remediation: None,
        diagnostics: json!({}),
    };
    backend.review_session_after_run(&config, &mut result).await;
    let spent = provider.received_requests().len() - received;
    (result, spent)
}

#[tokio::test]
async fn an_enabled_critic_attaches_its_findings() {
    let (result, _spent) = run_review_after(critic_config(true), finding_review_reply()).await;
    assert_eq!(result.session_findings.len(), 1, "{result:?}");
    assert_eq!(result.session_findings[0].kind, "unsupported_claim");
}

#[tokio::test]
async fn a_disabled_critic_leaves_the_result_unreviewed() {
    let (result, spent) = run_review_after(critic_config(false), finding_review_reply()).await;
    assert!(
        result.session_findings.is_empty(),
        "a disabled critic must not spend reviewer calls: {result:?}"
    );
    assert_eq!(spent, 0, "and must not reach the provider");
}

// ── end-to-end attempt loop ─────────────────────────────────────────────────

/// A revision on a non-final attempt is RETRIED with the feedback attached, and
/// the final approval resolves every issue ever raised.
#[tokio::test]
async fn a_revision_on_attempt_zero_is_retried_and_then_resolved() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            write_then_report()[0].clone(),
            write_then_report()[1].clone(),
            critic_reply("needs_revision", &["rename the helper"]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "report-2",
                liberado_executor::SUBMIT_REPORT_TOOL,
                json!({
                    "outcome": "succeeded",
                    "summary": "did the thing",
                    "artifacts": [],
                    "new_high_signal_facts": [],
                    "follow_up": null
                }),
            )]),
            critic_reply("acceptable", &[]),
        ],
    ));
    let backend = LiberadoLoopBackend::new(provider.clone());
    let mut request = base_request(dir.path());
    request.config.progress.max_attempts = 3;
    let traces = tempfile::tempdir().unwrap(); // outside the workspace
    request.config.trace_dir = Some(traces.path().to_string_lossy().into_owned());

    let result = backend.run(request).await.expect("run converges");
    assert_eq!(result.outcome, Outcome::Succeeded, "{result:?}");
    assert!(
        !result.diff_findings.is_empty(),
        "the raised issue is recorded as resolved: {result:?}"
    );
    // Two worker attempts (two turns each) + two critic calls happened.
    assert_eq!(provider.received_requests().len(), 5, "retry happened");
}

/// A revision on the FINAL attempt ends the run Failed and says so in the
/// summary — the repair router reads that line.
#[tokio::test]
async fn a_revision_on_the_final_attempt_fails_with_the_critic_summary() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            write_then_report()[0].clone(),
            write_then_report()[1].clone(),
            critic_reply("needs_revision", &["rename the helper"]),
        ],
    ));
    let backend = LiberadoLoopBackend::new(provider);
    let mut request = base_request(dir.path());
    request.config.progress.max_attempts = 1;
    let result = backend.run(request).await.expect("deliberate failure");
    assert_eq!(result.outcome, Outcome::Failed);
    assert!(
        result.summary.contains("critic requested revision"),
        "the summary must carry the refusal: {result:?}"
    );
}

// ── strategist machinery ────────────────────────────────────────────────────

/// With the gate on and one refutation past `strategist_after = 1`, the
/// strategist is consulted exactly once between the attempts — its reply is a
/// real model call in the transcript.
#[tokio::test]
async fn a_gate_refusion_consults_the_strategist_once() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            write_then_report()[0].clone(),
            write_then_report()[1].clone(),
            critic_reply("needs_revision", &["restructure"]),
            CompletionResponse::text("split config loading from the CLI entrypoint"),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "report-2",
                liberado_executor::SUBMIT_REPORT_TOOL,
                json!({
                    "outcome": "succeeded",
                    "summary": "did the thing",
                    "artifacts": [],
                    "new_high_signal_facts": [],
                    "follow_up": null
                }),
            )]),
            critic_reply("acceptable", &[]),
            critic_reply("acceptable", &[]),
        ],
    ));
    let backend = LiberadoLoopBackend::new(provider.clone());
    let mut request = base_request(dir.path());
    request.config.progress.max_attempts = 3;
    request.config.gate.enabled = true;
    request.config.gate.fresh_reviewers = 1;
    request.config.gate.strategist_after = 1;

    let result = backend.run(request).await.expect("run converges");
    assert_eq!(result.outcome, Outcome::Succeeded, "{result:?}");
    assert!(
        !result.gate_votes.is_empty(),
        "gate votes ride on the result: {result:?}"
    );
    // worker turns (2 + 1) + gatekeeper×2 + strategist×1
    assert_eq!(provider.received_requests().len(), 7);
    // The strategist's proposal rides into the NEXT attempt's goal text — a
    // counter that never advances means the directive is never proposed.
    let directive_reached_model = provider.received_requests()[2..].iter().any(|r| {
        r.messages.iter().any(|m| {
            m.content.contains("Structural directive") && m.content.contains("split config loading")
        })
    });
    assert!(
        directive_reached_model,
        "the directive must be proposed to the next attempt, not filed away"
    );
}

/// With the gate OFF the legacy single critic drives revisions, and the
/// strategist must stay out of it entirely.
#[tokio::test]
async fn a_disabled_gate_never_spends_a_strategist_call() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            write_then_report()[0].clone(),
            write_then_report()[1].clone(),
            critic_reply("needs_revision", &["rename the helper"]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "report-2",
                liberado_executor::SUBMIT_REPORT_TOOL,
                json!({
                    "outcome": "succeeded",
                    "summary": "did the thing",
                    "artifacts": [],
                    "new_high_signal_facts": [],
                    "follow_up": null
                }),
            )]),
            critic_reply("acceptable", &[]),
        ],
    ));
    let backend = LiberadoLoopBackend::new(provider.clone());
    let mut request = base_request(dir.path());
    request.config.progress.max_attempts = 3;
    request.config.gate.strategist_after = 1; // would fire if the gate were consulted

    let result = backend.run(request).await.expect("run converges");
    assert_eq!(result.outcome, Outcome::Succeeded, "{result:?}");
    // worker turns (2 + 1) + single critic×2 — no strategist call.
    assert_eq!(provider.received_requests().len(), 5);
}

/// A retryable validation failure spends another attempt instead of ending the
/// run — the second attempt clears the check by finishing the work.
#[tokio::test]
async fn a_retryable_validation_failure_gets_another_attempt() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            // Attempt 0 changes something else and reports success: the
            // marker-specific validation still fails.
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "write-1",
                "write_file",
                json!({"path": "other.txt", "content": "work
"}),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "report-1",
                liberado_executor::SUBMIT_REPORT_TOOL,
                json!({
                    "outcome": "succeeded",
                    "summary": "did the thing",
                    "artifacts": [],
                    "new_high_signal_facts": [],
                    "follow_up": null
                }),
            )]),
            // Attempt 1 writes the marker first, then reports.
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "write-2",
                "write_file",
                json!({"path": "marker.txt", "content": "done\n"}),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "report-2",
                liberado_executor::SUBMIT_REPORT_TOOL,
                json!({
                    "outcome": "succeeded",
                    "summary": "did the thing",
                    "artifacts": [],
                    "new_high_signal_facts": [],
                    "follow_up": null
                }),
            )]),
        ],
    ));
    let backend = LiberadoLoopBackend::new(provider);
    let mut request = base_request(dir.path());
    request.config.progress.max_attempts = 3;
    request.config.validation_command = Some(liberado_coder_core::CoderCommandConfig {
        program: "sh".into(),
        args: vec!["-c".into(), "test -f marker.txt".into()],
        env: Default::default(),
        timeout_secs: None,
        output_max_bytes: None,
    });
    request.config.critic.prompt = None;

    let result = backend.run(request).await.expect("second attempt lands");
    assert_eq!(result.outcome, Outcome::Succeeded, "{result:?}");
}
