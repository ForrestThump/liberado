//! Full mock end-to-end: criteria intake → freeze → apply → Liberado loop → verifier pipeline.
//!
//! Always safe for CI: no network, no real models, no cargo profiles. This is the ladder's first
//! rung before hybrid/live scaffolds in `live_scaffold.rs`.
//!
//! ```sh
//! cargo test -p liberado-coder-agent --test mock_intake_e2e
//! ```

mod common;

use common::{
    base_request, freeze_structural_todo, init_repo, intake_response, load_intake_outcome,
    mock_backend, mock_provider, write_incomplete_then_report, write_todo_scaffold_then_report,
};
use liberado_coder_agent::{
    freeze_if_ready, request_from_contract, run_intake, run_intake_until_ready, IntakeAnswer,
};
use liberado_coder_core::{CoderBackend, CoderError, FreezeAuthority, IntakeOutcome};
use liberado_common::Outcome;
use liberado_provider::CompletionResponse;

#[tokio::test]
async fn fixture_ready_freezes_applies_and_pipeline_passes() {
    let outcome = load_intake_outcome("intake_ready_todo_cli.json");
    let contract = freeze_if_ready(outcome, "goal-todo-1", FreezeAuthority::Human).unwrap();

    assert!(
        contract
            .draft
            .verifiers
            .iter()
            .any(|v| v.id() == "required_paths")
    );
    // Fixture deliberately omits verify_profile so mock path stays structural-only.
    assert!(
        !contract
            .draft
            .verifiers
            .iter()
            .any(|v| v.id() == "cargo-check"),
        "mock fixtures must not expand cargo profiles"
    );

    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let request = request_from_contract(&contract, base_request(dir.path()));
    assert!(request.task.description.contains("todo CLI"));
    assert_eq!(request.task.id, "goal-todo-1");
    assert!(!request.config.verifiers.is_empty());

    let backend = mock_backend(write_todo_scaffold_then_report());
    let result = backend.run(request).await.unwrap();

    assert_eq!(result.outcome, Outcome::Succeeded);
    assert!(result.files_changed.iter().any(|p| p.contains("main.rs")));
    let notes = result.validation_notes.expect("pipeline notes");
    assert!(notes.contains("required_paths") || notes.contains("main_fn") || notes.contains("has_diff"));
    assert!(dir.path().join("src/main.rs").is_file());
    let main = std::fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
    assert!(main.contains("fn main"));
}

#[tokio::test]
async fn mock_intake_provider_then_freeze_then_pipeline() {
    // One scripted intake turn (fixture JSON) → freeze → scripted worker.
    let intake = mock_provider([intake_response("intake_ready_todo_cli.json")]);
    let outcome = run_intake(
        intake.as_ref(),
        "make a small todo CLI with add/list",
        &[],
        None,
    )
    .await
    .unwrap();

    let contract = freeze_if_ready(outcome, "goal-todo-2", FreezeAuthority::Human).unwrap();

    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let request = request_from_contract(&contract, base_request(dir.path()));

    let backend = mock_backend(write_todo_scaffold_then_report());
    let result = backend.run(request).await.unwrap();
    assert_eq!(result.outcome, Outcome::Succeeded);
}

#[tokio::test]
async fn incomplete_worker_fails_frozen_structural_gates() {
    let contract = freeze_structural_todo("goal-incomplete");
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let request = request_from_contract(&contract, base_request(dir.path()));

    let backend = mock_backend(write_incomplete_then_report());
    let err = backend.run(request).await.unwrap_err();
    match err {
        CoderError::Validation(msg) => {
            assert!(
                msg.contains("required_paths")
                    || msg.contains("Cargo.toml")
                    || msg.contains("main.rs")
                    || msg.contains("missing"),
                "unexpected validation feedback: {msg}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }
}

#[tokio::test]
async fn clarify_then_answer_then_freeze_then_pipeline() {
    let intake = mock_provider([
        intake_response("intake_needs_clarification.json"),
        intake_response("intake_ready_after_answers.json"),
    ]);

    let (outcome, answers) = run_intake_until_ready(
        intake.as_ref(),
        "make a todo CLI",
        |questions| {
            assert!(!questions.is_empty());
            vec![
                IntakeAnswer {
                    question_id: "stack".into(),
                    answer: "Rust".into(),
                },
                IntakeAnswer {
                    question_id: "store".into(),
                    answer: "local JSON file".into(),
                },
            ]
        },
        None,
        3,
    )
    .await
    .unwrap();

    assert_eq!(answers.len(), 2);
    match &outcome {
        IntakeOutcome::ReadyForFreeze { draft, .. } => {
            assert!(draft.description.to_lowercase().contains("rust"));
        }
        other => panic!("expected ready after answers, got {other:?}"),
    }

    let contract = freeze_if_ready(outcome, "goal-clarify", FreezeAuthority::Human).unwrap();
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let request = request_from_contract(&contract, base_request(dir.path()));
    let backend = mock_backend(write_todo_scaffold_then_report());
    let result = backend.run(request).await.unwrap();
    assert_eq!(result.outcome, Outcome::Succeeded);

    // Intake model was asked twice (clarify + ready).
    assert_eq!(intake.received_requests().len(), 2);
}

#[tokio::test]
async fn cannot_freeze_while_still_needs_clarification() {
    let intake = mock_provider([intake_response("intake_needs_clarification.json")]);
    let outcome = run_intake(intake.as_ref(), "todo app", &[], None)
        .await
        .unwrap();
    let err = freeze_if_ready(outcome, "g", FreezeAuthority::Human).unwrap_err();
    assert!(err.to_string().contains("clarification"));
}

#[tokio::test]
async fn fixtures_round_trip_as_intake_outcome() {
    for name in [
        "intake_ready_todo_cli.json",
        "intake_needs_clarification.json",
        "intake_ready_after_answers.json",
    ] {
        let outcome = load_intake_outcome(name);
        let again: IntakeOutcome =
            serde_json::from_str(&serde_json::to_string(&outcome).unwrap()).unwrap();
        assert_eq!(outcome, again, "fixture {name} should round-trip");
    }
}

/// Guard: if someone adds cargo to the ready fixture, CI mock e2e must stay structural.
#[test]
fn ready_fixture_has_no_command_verifiers() {
    let outcome = load_intake_outcome("intake_ready_todo_cli.json");
    let IntakeOutcome::ReadyForFreeze { draft, .. } = outcome else {
        panic!("expected ready fixture");
    };
    assert!(draft.verify_profile.is_none());
    for v in &draft.verifiers {
        assert_ne!(
            v.kind(),
            "command",
            "mock fixture must not use command verifiers: {}",
            v.id()
        );
    }
}

/// Sanity: mock provider exhausted mid-pipeline fails loudly (no silent hang).
#[tokio::test]
async fn exhausted_worker_script_surfaces_error() {
    let contract = freeze_structural_todo("goal-exhausted");
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let request = request_from_contract(&contract, base_request(dir.path()));
    // One response only — not enough for write + report.
    let backend = mock_backend([CompletionResponse::tool_calls(vec![
        liberado_provider::ToolInvocation::new(
            "write-1",
            "write_file",
            serde_json::json!({"path": "Cargo.toml", "content": "[package]\nname=\"x\"\nversion=\"0.1.0\"\nedition=\"2021\"\n"}),
        ),
    ])]);
    let err = backend.run(request).await.unwrap_err();
    // Provider MockExhausted bubbles as Provider or Backend error.
    let msg = err.to_string();
    assert!(
        msg.contains("MockExhausted")
            || msg.contains("exhausted")
            || msg.contains("provider")
            || matches!(err, CoderError::Provider(_)),
        "unexpected error: {msg}"
    );
}
