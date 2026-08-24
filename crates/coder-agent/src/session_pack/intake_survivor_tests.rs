//! Split from `session_pack/intake.rs`: kills the baseline campaign's
//! survivors.
//!
//! Covers resume detection from the transcript, the coherence-redraft budget,
//! clarify-round boundaries, intake context assembly, and the freeze-rendering
//! sections.

use super::*;
use liberado_provider::CompletionResponse;
use liberado_session::{GoalSessionRecord, GoalSessionStore, HumanInput, SessionRecordStore};
use std::path::PathBuf;
use std::sync::Arc;

fn pack() -> CodingSessionPack {
    CodingSessionPack::new(
        Arc::new(liberado_provider::MockProvider::new("mock")),
        std::env::temp_dir(),
    )
}

fn settings(max_clarify_rounds: u32) -> IntakeSettings {
    IntakeSettings {
        enabled: true,
        max_clarify_rounds,
    }
}

fn goal_from(payload: serde_json::Value) -> GoalSpec {
    serde_json::from_value(serde_json::json!({
        "description": "build a todo cli",
        "payload": payload
    }))
    .unwrap()
}

struct Transcript {
    store: Arc<GoalSessionStore>,
    grant: liberado_session::SessionGrant,
}

impl Transcript {
    async fn open() -> Self {
        let store = Arc::new(GoalSessionStore::new());
        let mut spec = goal_from(serde_json::json!({}));
        spec.id = Some("s1".into());
        SessionRecordStore::insert(store.as_ref(), GoalSessionRecord::new(spec)).await;
        Self {
            store,
            grant: liberado_session::SessionGrant::default(),
        }
    }
    fn ctx(&self) -> PackContext<'_> {
        PackContext::new(&self.grant, self.store.clone(), "s1")
    }
}

async fn inputs_with(
    answers: &[&str],
) -> (
    InputChannel,
    tokio::sync::watch::Receiver<bool>,
    tokio::sync::watch::Sender<bool>,
) {
    let (in_tx, in_rx) = tokio::sync::mpsc::channel::<HumanInput>(8);
    for a in answers {
        in_tx.send(HumanInput::new(*a)).await.unwrap();
    }
    drop(in_tx);
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    (InputChannel::new(in_rx, None), cancel_rx, cancel_tx)
}

fn cmd(id: &str) -> liberado_coder_core::VerifierSpec {
    liberado_coder_core::VerifierSpec::Command {
        id: id.into(),
        program: "cargo".into(),
        args: vec!["clippy".into()],
        env: Default::default(),
        timeout_secs: None,
        output_max_bytes: None,
        network: false,
    }
}

fn plain_draft() -> GoalContractDraft {
    GoalContractDraft {
        description: "build a todo cli".into(),
        success_criteria: vec!["it works".into()],
        verifiers: vec![],
        out_of_scope: vec![],
        assumed_defaults: vec![],
        domain_hint: None,
        verify_profile: None,
    }
}

fn contradictory_draft() -> GoalContractDraft {
    GoalContractDraft {
        verifiers: vec![cmd("cargo-clippy")],
        out_of_scope: vec!["No clippy or fmt checks.".into()],
        ..plain_draft()
    }
}

// ── build_intake_context ────────────────────────────────────────────────────

#[test]
fn context_includes_payload_context_project_and_root() {
    let goal = goal_from(serde_json::json!({
        "context": "the vault layout",
        "project": "probe",
        "workspace_root": "/ws/probe"
    }));
    let context = build_intake_context(&goal).expect("something to say");
    assert!(context.contains("the vault layout"), "{context}");
    assert!(
        context.contains("Authorized coding project name: `probe`"),
        "{context}"
    );
    assert!(context.contains("Authorized workspace_root"), "{context}");
}

#[test]
fn an_empty_payload_needs_no_context() {
    assert_eq!(
        build_intake_context(&goal_from(serde_json::json!({}))),
        None
    );
}

#[test]
fn a_whitespace_context_counts_as_nothing() {
    let goal = goal_from(serde_json::json!({ "context": "   \n\t" }));
    assert_eq!(build_intake_context(&goal), None);
}

// ── render_draft ────────────────────────────────────────────────────────────

#[test]
fn verifiers_are_shown_with_their_judgement_role() {
    let rendered = render_draft(
        &GoalContractDraft {
            verifiers: vec![cmd("cargo-clippy")],
            verify_profile: Some("rust-strict".into()),
            ..plain_draft()
        },
        "",
    );
    assert!(rendered.contains("Verifiers ("), "{rendered}");
    assert!(rendered.contains("added by verify_profile"), "{rendered}");
}

#[test]
fn no_verifiers_means_no_verifier_section() {
    let rendered = render_draft(&plain_draft(), "");
    assert!(!rendered.contains("Verifiers ("), "{rendered}");
}

#[test]
fn contradictions_get_their_own_section_not_warnings() {
    let rendered = render_draft(&contradictory_draft(), "");
    assert!(rendered.contains('⛔'), "{rendered}");
    assert!(!rendered.contains('⚠'), "{rendered}");
}

#[test]
fn scope_sections_render_when_present_and_stay_absent_when_empty() {
    let mut draft = plain_draft();
    draft.out_of_scope = vec!["no refactors".into()];
    draft.assumed_defaults = vec!["rust edition 2021".into()];
    let rendered = render_draft(&draft, "");
    assert!(
        rendered.contains("Out of scope:\n  - no refactors"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Assumed (not asked):\n  - rust edition 2021"),
        "{rendered}"
    );

    let bare = render_draft(&plain_draft(), "");
    assert!(!bare.contains("Out of scope"), "{bare}");
    assert!(!bare.contains("Assumed"), "{bare}");
}

#[test]
fn rationale_is_quoted_only_when_it_says_something() {
    let quiet = render_draft(&plain_draft(), "   ");
    assert!(!quiet.contains("Why these checks"), "{quiet}");
    let loud = render_draft(&plain_draft(), "profile gates are stricter here");
    assert!(
        loud.contains("Why these checks: profile gates are stricter"),
        "{loud}"
    );
}

// ── round accounting ────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn intake_ctx<'a>(
    pack: &'a CodingSessionPack,
    goal: &'a GoalSpec,
    set: &'a IntakeSettings,
    events: &'a tokio::sync::mpsc::Sender<SessionEvent>,
    inputs: &'a mut InputChannel,
    cancel: &'a mut tokio::sync::watch::Receiver<bool>,
    _transcript: &'a Transcript,
    ctx: &'a PackContext<'a>,
) -> IntakeCtx<'a> {
    let _ = pack;
    IntakeCtx {
        session_id: "s1",
        goal,
        ctx,
        settings: set,
        events,
        inputs,
        cancel,
    }
}

/// At exactly the redraft cap the model stops burning its own retries and the
/// human becomes the backstop.
#[tokio::test]
async fn coherence_redrafts_stop_at_the_cap_and_ask_the_human() {
    let pack = pack();
    let transcript = Transcript::open().await;
    let goal = goal_from(serde_json::json!({}));
    let set = settings(2);
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let (mut inputs, mut cancel, _keep) = inputs_with(&["reject"]).await;
    let ctx = transcript.ctx();
    let mut c = intake_ctx(
        &pack,
        &goal,
        &set,
        &tx,
        &mut inputs,
        &mut cancel,
        &transcript,
        &ctx,
    )
    .await;
    let mut state = IntakeLoop {
        answers: vec![],
        rounds: 0,
        coherence_redrafts: MAX_COHERENCE_REDRAFTS,
    };
    // A contradictory draft cannot freeze, so the human's way out is rejection.
    let step = pack
        .handle_ready_for_freeze(&mut c, &mut state, contradictory_draft(), String::new())
        .await
        .unwrap();
    assert!(
        matches!(step, IntakeStep::Finish(IntakePhase::Rejected)),
        "at the cap the human decides"
    );
}

/// Inside the cap a contradiction spends the MODEL's budget, not the human's:
/// the counter moves and the fix request is pushed as machine feedback.
#[tokio::test]
async fn a_contradiction_sends_the_draft_back_without_spending_a_clarify_round() {
    let pack = pack();
    let transcript = Transcript::open().await;
    let goal = goal_from(serde_json::json!({}));
    let set = settings(2);
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let (mut inputs, mut cancel, _keep) = inputs_with(&[]).await;
    let ctx = transcript.ctx();
    let mut c = intake_ctx(
        &pack,
        &goal,
        &set,
        &tx,
        &mut inputs,
        &mut cancel,
        &transcript,
        &ctx,
    )
    .await;
    let mut state = IntakeLoop {
        answers: vec![],
        rounds: 0,
        coherence_redrafts: 0,
    };
    let step = pack
        .handle_ready_for_freeze(&mut c, &mut state, contradictory_draft(), String::new())
        .await
        .unwrap();
    assert!(matches!(step, IntakeStep::Continue), "expected continue");
    assert_eq!(state.coherence_redrafts, 1, "the model's own budget pays");
    assert_eq!(state.rounds, 0, "the human's budget is untouched");
    assert_eq!(state.answers.len(), 1);
    assert_eq!(state.answers[0].question_id, "coherence");
}

/// One revision below the cap keeps negotiating instead of giving up.
#[tokio::test]
async fn a_revision_below_the_round_limit_keeps_going() {
    let pack = pack();
    let transcript = Transcript::open().await;
    let goal = goal_from(serde_json::json!({}));
    let set = settings(2);
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let (mut inputs, mut cancel, _keep) = inputs_with(&["add a test for the parser"]).await;
    let ctx = transcript.ctx();
    let mut c = intake_ctx(
        &pack,
        &goal,
        &set,
        &tx,
        &mut inputs,
        &mut cancel,
        &transcript,
        &ctx,
    )
    .await;
    let mut state = IntakeLoop {
        answers: vec![],
        rounds: 1,
        coherence_redrafts: 0,
    };
    let step = pack
        .handle_ready_for_freeze(&mut c, &mut state, plain_draft(), String::new())
        .await
        .unwrap();
    assert!(matches!(step, IntakeStep::Continue), "expected continue");
    assert_eq!(state.rounds, 2);
    assert_eq!(state.answers.last().unwrap().question_id, "revision");
}

/// The revision AFTER the cap ends the phase with the draft preserved.
#[tokio::test]
async fn a_revision_past_the_round_limit_goes_to_review() {
    let pack = pack();
    let transcript = Transcript::open().await;
    let goal = goal_from(serde_json::json!({}));
    let set = settings(2);
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let (mut inputs, mut cancel, _keep) = inputs_with(&["still wrong"]).await;
    let ctx = transcript.ctx();
    let mut c = intake_ctx(
        &pack,
        &goal,
        &set,
        &tx,
        &mut inputs,
        &mut cancel,
        &transcript,
        &ctx,
    )
    .await;
    let mut state = IntakeLoop {
        answers: vec![],
        rounds: 2,
        coherence_redrafts: 0,
    };
    let step = pack
        .handle_ready_for_freeze(&mut c, &mut state, plain_draft(), String::new())
        .await
        .unwrap();
    match step {
        IntakeStep::Finish(IntakePhase::NeedsReview(Some(draft))) => {
            assert_eq!(draft.description, "build a todo cli");
        }
        _other => panic!("expected review hand-off with the draft"),
    }
}

/// Clarification spending follows the same boundary: at the limit there is no
/// fourth question.
#[tokio::test]
async fn clarification_at_the_limit_hands_off_to_review() {
    let pack = pack();
    let transcript = Transcript::open().await;
    let goal = goal_from(serde_json::json!({}));
    let set = settings(2);
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let (mut inputs, mut cancel, _keep) = inputs_with(&[]).await;
    let ctx = transcript.ctx();
    let mut c = intake_ctx(
        &pack,
        &goal,
        &set,
        &tx,
        &mut inputs,
        &mut cancel,
        &transcript,
        &ctx,
    )
    .await;
    let mut state = IntakeLoop {
        answers: vec![],
        rounds: 2,
        coherence_redrafts: 0,
    };
    let questions = vec![liberado_coder_core::IntakeQuestion {
        id: "q1".into(),
        prompt: "which crate?".into(),
        options: vec![],
        affects: String::new(),
    }];
    let step = pack
        .handle_clarification(&mut c, &mut state, questions, None)
        .await
        .unwrap();
    assert!(
        matches!(step, IntakeStep::Finish(IntakePhase::NeedsReview(None))),
        "expected hand-off to review"
    );
}

#[tokio::test]
async fn clarification_below_the_limit_asks_the_question() {
    let pack = pack();
    let transcript = Transcript::open().await;
    let goal = goal_from(serde_json::json!({}));
    let set = settings(2);
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let (mut inputs, mut cancel, _keep) = inputs_with(&["the server crate"]).await;
    let ctx = transcript.ctx();
    let mut c = intake_ctx(
        &pack,
        &goal,
        &set,
        &tx,
        &mut inputs,
        &mut cancel,
        &transcript,
        &ctx,
    )
    .await;
    let mut state = IntakeLoop {
        answers: vec![],
        rounds: 1,
        coherence_redrafts: 0,
    };
    let questions = vec![liberado_coder_core::IntakeQuestion {
        id: "q1".into(),
        prompt: "which crate?".into(),
        options: vec![],
        affects: "chooses the workspace".into(),
    }];
    let step = pack
        .handle_clarification(&mut c, &mut state, questions, None)
        .await
        .unwrap();
    assert!(matches!(step, IntakeStep::Continue), "expected continue");
    assert_eq!(state.rounds, 2);
    assert_eq!(state.answers[0].answer, "the server crate");
}

/// A resume with answered questions says so; a fresh start stays quiet.
#[tokio::test]
async fn resuming_with_prior_answers_announces_the_pickup() {
    let dir = tempfile::tempdir().unwrap();
    let _env = crate::DATA_DIR_ENV_LOCK.lock().await;
    let data = tempfile::tempdir().unwrap();
    struct RestoreDataDir(Option<std::ffi::OsString>);
    impl Drop for RestoreDataDir {
        fn drop(&mut self) {
            unsafe {
                match &self.0 {
                    Some(v) => std::env::set_var("LIBERADO_DATA_DIR", v),
                    None => std::env::remove_var("LIBERADO_DATA_DIR"),
                }
            }
        }
    }
    let prior = std::env::var_os("LIBERADO_DATA_DIR");
    unsafe {
        std::env::set_var("LIBERADO_DATA_DIR", data.path());
    }
    let _restore = RestoreDataDir(prior);

    let transcript = Transcript::open().await;
    // Seed a prior negotiation: goal turn, then a question and its answer.
    transcript
        .ctx()
        .record_turn(liberado_session::TurnAuthor::User, "build a todo cli")
        .await;
    transcript
        .ctx()
        .record_turn(liberado_session::TurnAuthor::Assistant, "which crate?")
        .await;
    transcript
        .ctx()
        .record_turn(liberado_session::TurnAuthor::User, "the server crate")
        .await;

    let provider = liberado_provider::MockProvider::with_script(
        "mock",
        [CompletionResponse::text(
            r#"{"status":"ready_for_freeze","draft":{"description":"todo cli","success_criteria":["works"]},"rationale":"clear enough"}"#,
        )],
    );
    let pack = CodingSessionPack::new(Arc::new(provider), std::env::temp_dir());
    let goal = goal_from(serde_json::json!({}));
    let set = settings(2);
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let (mut inputs, mut cancel, _keep) = inputs_with(&["accept"]).await;
    let ctx = transcript.ctx();

    let phase = pack
        .run_intake_phase("s1", &goal, &ctx, &set, &tx, &mut inputs, &mut cancel)
        .await
        .expect("intake completes");
    assert!(matches!(phase, IntakePhase::Frozen(_)), "{phase:?}");

    let mut saw_resumed = false;
    while let Ok(event) = rx.try_recv() {
        if let SessionEventKind::Progress { message } = event.kind
            && message.contains("resumed:")
            && message.contains("prior answer(s)")
        {
            saw_resumed = true;
        }
    }
    assert!(saw_resumed, "the resume pickup must be announced");

    let _ = dir; // keep the tempdir alive for the store's lifetime
    let _: PathBuf = PathBuf::new();
}
