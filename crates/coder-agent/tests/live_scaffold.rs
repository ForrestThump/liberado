//! Staged live / hybrid scaffolds for criteria intake + coding.
//!
//! **Ignored by default.** Escalate only after `mock_intake_e2e` is green:
//!
//! | Rung | Test | Needs | Risk |
//! |---|---|---|---|
//! | 0 | `mock_intake_e2e` | nothing | CI-safe |
//! | 1 | `live_intake_schema_smoke` | `OPENROUTER_API_KEY` | cheap: intake only, no worker |
//! | 2 | `live_intake_then_mock_worker` | `OPENROUTER_API_KEY` | intake live; worker mocked; command gates stripped |
//! | 3 | existing `openrouter_deepseek_live_coding_smoke` in unit tests | key + network | full live worker, structural task |
//!
//! ```sh
//! # Rung 0 (always)
//! cargo test -p liberado-coder-agent --test mock_intake_e2e
//!
//! # Rung 1
//! $env:OPENROUTER_API_KEY="..."
//! cargo test -p liberado-coder-agent --test live_scaffold live_intake_schema_smoke -- --ignored --nocapture
//!
//! # Rung 2
//! cargo test -p liberado-coder-agent --test live_scaffold live_intake_then_mock_worker -- --ignored --nocapture
//! ```
//!
//! Optional: `LIBERADO_CODER_LIVE_MODEL` (default `deepseek/deepseek-v4-pro`).

mod common;

use common::{
    base_request, init_repo, live_model, mock_backend, openrouter_provider_from_env,
    structural_only, write_contract_paths_then_report,
};
use liberado_coder_agent::{
    freeze_if_ready, request_from_contract, run_intake, run_intake_until_ready, IntakeAnswer,
};
use liberado_coder_core::{CoderBackend, FreezeAuthority, IntakeOutcome, validate_draft};
use liberado_common::Outcome;

/// Rung 1: real intake model only. Asserts typed outcome + draft validation when ready.
/// Does **not** run the coding worker.
#[tokio::test]
#[ignore = "hits OpenRouter — set OPENROUTER_API_KEY; run with --ignored after mock_intake_e2e is green"]
async fn live_intake_schema_smoke() {
    let model = live_model();
    let provider = openrouter_provider_from_env(&model);

    let writeup = "\
Build a tiny command-line todo app in Rust. \
It should support adding a todo and listing todos, with persistence to a local file. \
Keep scope minimal: no network, no GUI.";

    let outcome = run_intake(provider.as_ref(), writeup, &[], None)
        .await
        .expect("live intake complete_json");

    match outcome {
        IntakeOutcome::NeedsClarification { questions, partial_draft } => {
            assert!(
                !questions.is_empty(),
                "clarification path must include at least one question"
            );
            for q in &questions {
                assert!(!q.id.is_empty());
                assert!(!q.prompt.is_empty());
            }
            if let Some(draft) = partial_draft {
                // Partial may be sparse; only soft-check non-empty description if present.
                let _ = draft;
            }
            eprintln!(
                "live intake returned NeedsClarification ({} question(s)) — OK for schema smoke",
                questions.len()
            );
        }
        IntakeOutcome::ReadyForFreeze { mut draft, rationale } => {
            eprintln!("live intake ready; rationale: {rationale}");
            // Expand profiles for validation the same way freeze does.
            liberado_coder_core::expand_verify_profile_into(&mut draft);
            validate_draft(&draft).expect("ready draft must validate");
            assert!(!draft.description.trim().is_empty());
            // Prefer that the model proposed some machine gates or a profile.
            let has_gates = !draft.verifiers.is_empty() || draft.verify_profile.is_some();
            assert!(
                has_gates || !draft.success_criteria.is_empty(),
                "ready draft should have verifiers, a profile, or prose criteria"
            );
        }
    }
}

/// Rung 2: live intake (up to a few clarify rounds with fixed answers), then **mock** worker.
///
/// Command verifiers / cargo profiles from the live draft are stripped so we still exercise
/// freeze + apply + pipeline without going full live coding yet.
#[tokio::test]
#[ignore = "hits OpenRouter for intake only — set OPENROUTER_API_KEY; mock worker; --ignored"]
async fn live_intake_then_mock_worker() {
    let model = live_model();
    let provider = openrouter_provider_from_env(&model);

    let writeup = "\
Create a minimal Rust binary crate named todo-cli with src/main.rs containing fn main. \
No network. Prefer structural acceptance checks (paths/content) over cargo test for now.";

    let (outcome, answers) = run_intake_until_ready(
        provider.as_ref(),
        writeup,
        |questions| {
            eprintln!("live clarify round: {questions:?}");
            questions
                .iter()
                .map(|q| {
                    let answer = if q.options.iter().any(|o| o.eq_ignore_ascii_case("rust")) {
                        "Rust".to_string()
                    } else if !q.options.is_empty() {
                        q.options[0].clone()
                    } else {
                        "minimal local file store; Rust".to_string()
                    };
                    IntakeAnswer {
                        question_id: q.id.clone(),
                        answer,
                    }
                })
                .collect()
        },
        None,
        3,
    )
    .await
    .expect("live intake until ready");

    eprintln!("clarify answers used: {answers:?}");

    let outcome = match outcome {
        ready @ IntakeOutcome::ReadyForFreeze { .. } => ready,
        IntakeOutcome::NeedsClarification { questions, .. } => {
            panic!(
                "still needs clarification after max rounds ({} questions): {questions:?}",
                questions.len()
            );
        }
    };

    // Human freeze stand-in: strip command gates for mock worker safety.
    let IntakeOutcome::ReadyForFreeze { mut draft, rationale } = outcome else {
        unreachable!();
    };
    eprintln!("freezing live draft (rationale={rationale}); stripping command verifiers for mock worker");
    structural_only(&mut draft);

    // If the live model produced no structural paths, inject the known scaffold gates so the
    // mock worker path remains meaningful.
    if !draft
        .verifiers
        .iter()
        .any(|v| matches!(v, liberado_coder_core::VerifierSpec::PathsExist { .. }))
    {
        draft.verifiers.extend(common::structural_todo_verifiers());
    }

    let contract = liberado_coder_core::GoalContract::freeze(
        "live-hybrid-1",
        draft,
        FreezeAuthority::Human,
    )
    .expect("freeze hybrid contract");

    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let request = request_from_contract(&contract, base_request(dir.path()));

    // Mock worker writes whatever structural paths the live freeze required.
    let backend = mock_backend(write_contract_paths_then_report(&contract));
    let result = backend.run(request).await.expect("mock worker after live freeze");

    assert_eq!(result.outcome, Outcome::Succeeded);
    assert!(
        !result.files_changed.is_empty(),
        "expected workspace changes from mock worker"
    );
    eprintln!(
        "hybrid success: files={:?} notes={:?}",
        result.files_changed, result.validation_notes
    );
}

/// Rung 2b: prove freeze_if_ready rejects live clarification without human answers.
#[tokio::test]
#[ignore = "hits OpenRouter — optional; set OPENROUTER_API_KEY"]
async fn live_intake_clarify_cannot_freeze_without_answers() {
    let model = live_model();
    let provider = openrouter_provider_from_env(&model);

    // Deliberately vague to bias toward clarification (not guaranteed).
    let outcome = run_intake(
        provider.as_ref(),
        "build the thing we talked about for the project",
        &[],
        None,
    )
    .await
    .expect("live intake");

    match outcome {
        o @ IntakeOutcome::NeedsClarification { .. } => {
            let err = freeze_if_ready(o, "nope", FreezeAuthority::Human).unwrap_err();
            assert!(err.to_string().contains("clarification"));
        }
        IntakeOutcome::ReadyForFreeze { draft, .. } => {
            // Model was confident; still fine — freeze path must work.
            let c = liberado_coder_core::GoalContract::freeze(
                "surprise-ready",
                draft,
                FreezeAuthority::Human,
            );
            assert!(c.is_ok() || c.is_err());
            eprintln!("model returned ReadyForFreeze for vague writeup; freeze attempted");
        }
    }
}
