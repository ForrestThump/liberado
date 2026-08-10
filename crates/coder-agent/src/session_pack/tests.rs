//! Tests for the coding session pack — intake, the build attempt loop, the ask seam, and resume.

use super::intake::{IntakePhase, IntakeSettings, answers_from_transcript, render_draft};
use super::*;
use async_trait::async_trait;
use liberado_coder_core::{
    CoderBackend, CoderError, CoderRunRequest, CoderRunResult, FreezeAuthority, GoalContract,
    GoalContractDraft, IntakeOutcome, SandboxSpec, VerifierSpec,
};
use liberado_common::Outcome;
use liberado_provider::{CompletionResponse, MockProvider};
use liberado_session::HumanInput;
use tokio::sync::mpsc;

fn ready_json(description: &str) -> String {
    serde_json::to_string(&IntakeOutcome::ReadyForFreeze {
        draft: GoalContractDraft {
            description: description.into(),
            success_criteria: vec!["add and list work".into()],
            verifiers: vec![VerifierSpec::PathsExist {
                id: "paths".into(),
                paths: vec!["src/main.rs".into()],
            }],
            out_of_scope: vec!["network".into()],
            assumed_defaults: vec!["Rust".into()],
            domain_hint: Some("coding".into()),
            verify_profile: None,
        },
        rationale: "the stack is clear".into(),
    })
    .unwrap()
}

const CLARIFY_JSON: &str = r#"{
        "status": "needs_clarification",
        "questions": [{"id":"stack","prompt":"Rust or Node?","options":["Rust","Node"],"affects":"verify profile"}]
    }"#;

/// A pack whose intake model replays `script`, plus a pre-loaded human input channel. Human
/// answers are buffered, so the pack consumes them in order at each await point.
fn harness(
    script: Vec<&str>,
    human: Vec<&str>,
) -> (
    CodingSessionPack,
    Sender<SessionEvent>,
    mpsc::Receiver<SessionEvent>,
    InputChannel,
    tokio::sync::watch::Receiver<bool>,
    // The cancel *sender* must be handed back and kept alive: dropping it makes
    // `cancel.changed()` resolve immediately, which races `inputs.recv()` in the pack's
    // `select!` and cancels the session at random. Tests bind it to keep the session live.
    tokio::sync::watch::Sender<bool>,
) {
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        script
            .into_iter()
            .map(CompletionResponse::text)
            .collect::<Vec<_>>(),
    ));
    let pack = CodingSessionPack::new(provider, std::env::temp_dir());

    let (ev_tx, ev_rx) = mpsc::channel::<SessionEvent>(64);
    let (in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
    for h in human {
        in_tx.try_send(HumanInput::new(h)).unwrap();
    }
    drop(in_tx); // an await with no answer left closes rather than hanging the test
    let inputs = InputChannel::new(in_rx, None);
    let (c_tx, c_rx) = tokio::sync::watch::channel(false);
    (pack, ev_tx, ev_rx, inputs, c_rx, c_tx)
}

fn goal(description: &str) -> GoalSpec {
    GoalSpec {
        id: None,
        description: description.into(),
        success_criteria: vec![],
        domain: liberado_session::DomainHint::Coding,
        max_turns: 0,
        max_idle_secs: None,
        origin: None,
        profile: None,
        payload: serde_json::json!({}),
    }
}

fn settings(max_clarify_rounds: u32) -> IntakeSettings {
    IntakeSettings {
        enabled: true,
        max_clarify_rounds,
    }
}

/// A real (in-memory) store with session `s1` open, plus the grant a `PackContext` borrows.
/// Turns the pack records actually land here, so a test can assert the transcript — which is the
/// whole point of S7's dialogue becoming turns rather than events.
struct Transcript {
    store: Arc<liberado_session::GoalSessionStore>,
    grant: liberado_session::SessionGrant,
}

impl Transcript {
    async fn open() -> Self {
        let store = Arc::new(liberado_session::GoalSessionStore::new());
        // The session must be open under the SAME id the pack records against, or every turn is
        // dropped on the floor — which is exactly what a store does with a turn for a session it
        // has never heard of.
        let mut spec = goal("make a todo cli");
        spec.id = Some("s1".into());
        liberado_session::SessionRecordStore::insert(
            store.as_ref(),
            liberado_session::GoalSessionRecord::new(spec),
        )
        .await;
        Self {
            store,
            grant: liberado_session::SessionGrant::default(),
        }
    }
    fn ctx(&self) -> PackContext<'_> {
        PackContext::new(&self.grant, self.store.clone(), "s1")
    }
}

/// A backend that fails the first attempt and succeeds on the next, recording every request it
/// was handed. The recording is the point: it is the only way to prove a human's mid-build
/// answer actually reaches the *backend* rather than merely being narrated to the event bus.
struct ScriptedBackend {
    seen: Arc<std::sync::Mutex<Vec<CoderRunRequest>>>,
    fail_attempts: u32,
}

#[async_trait]
impl CoderBackend for ScriptedBackend {
    fn name(&self) -> &str {
        "scripted"
    }
    async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
        let attempt = request.attempt;
        self.seen.lock().unwrap().push(request);
        let failed = attempt < self.fail_attempts;
        Ok(CoderRunResult {
            backend: "scripted".into(),
            outcome: if failed {
                Outcome::Failed
            } else {
                Outcome::Succeeded
            },
            summary: if failed {
                "verifier `tests` failed".into()
            } else {
                "green".into()
            },
            files_changed: vec![],
            file_changes: Vec::new(),
            validation_notes: None,
            critic_verdict: None,
            gate_votes: Vec::new(),
            trace_path: None,
            diff_findings: Vec::new(),
            session_findings: Vec::new(),
            remediation: None,
            diagnostics: serde_json::json!({}),
        })
    }
}

#[tokio::test]
async fn a_failed_build_asks_the_human_and_retries_with_their_answer() {
    // The point of the whole exercise (one-execution-engine E5): a goal-pursuing session that
    // hits a wall stops, asks, waits, and then *uses the answer*. Recording the guidance in the
    // transcript and failing anyway would look identical on the event bus and be worthless — so
    // this asserts the guidance arrives in the backend's second attempt as `prior_feedback`.
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let backend = Arc::new(ScriptedBackend {
        seen: seen.clone(),
        fail_attempts: 1,
    });
    let provider = Arc::new(MockProvider::with_script("mock", vec![]));
    let pack = CodingSessionPack::with_backend(backend, provider, std::env::temp_dir());

    let (ev_tx, _ev_rx) = mpsc::channel::<SessionEvent>(64);
    let (in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
    in_tx
        .try_send(HumanInput::new("pin serde to 1.0 and rerun"))
        .unwrap();
    drop(in_tx);
    let inputs = InputChannel::new(in_rx, None);
    let (_c_tx, cancel) = tokio::sync::watch::channel(false);

    let workspace = std::env::temp_dir().join("liberado-e5-retry-test");
    std::fs::create_dir_all(&workspace).unwrap();

    // Intake off (this test is about the *build* loop), AskHuman on (so the pack may stop).
    let mut g = goal("make a todo cli");
    g.payload = serde_json::json!({
        "workspace_root": workspace.to_string_lossy(),
        "intake": { "enabled": false },
        "force_host_local": true,
    });

    let store = Arc::new(liberado_session::GoalSessionStore::new());
    let mut spec = g.clone();
    spec.id = Some("s1".into());
    liberado_session::SessionRecordStore::insert(
        store.as_ref(),
        liberado_session::GoalSessionRecord::new(spec),
    )
    .await;
    let grant = liberado_session::SessionGrant {
        capabilities: [Capability::AskHuman].into_iter().collect(),
        ..Default::default()
    };
    let ctx = PackContext::new(&grant, store.clone(), "s1");

    let out = pack
        .run("s1", &g, &ctx, ev_tx, inputs, cancel)
        .await
        .unwrap();

    let requests = seen.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        2,
        "the pack must actually re-run the backend after the human answers, not just record \
             the answer and fail: {out:?}"
    );
    assert_eq!(requests[0].attempt, 0);
    assert_eq!(requests[1].attempt, 1, "the retry is a new attempt");
    assert!(
        requests[1]
            .prior_feedback
            .iter()
            .any(|f| f.contains("pin serde to 1.0")),
        "the human's guidance must reach the backend as feedback: {:#?}",
        requests[1].prior_feedback
    );
    assert_eq!(
        out.terminal,
        TerminalKind::Succeeded,
        "the guided retry succeeded, so the session succeeded"
    );
}

/// A backend that gets **stuck** (`Err(NoChanges)`) on its first attempt rather than returning a
/// failed verdict, then succeeds once it has been told something. This is the shape a real run
/// produces when the model cannot make progress — and the shape `ScriptedBackend` never made,
/// which is precisely why the ask seam shipped on the `Ok` path only and the live test caught it.
struct StuckBackend {
    seen: Arc<std::sync::Mutex<Vec<CoderRunRequest>>>,
}

#[async_trait]
impl CoderBackend for StuckBackend {
    fn name(&self) -> &str {
        "stuck"
    }
    async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
        let attempt = request.attempt;
        self.seen.lock().unwrap().push(request);
        if attempt == 0 {
            return Err(CoderError::NoChanges);
        }
        Ok(CoderRunResult {
            backend: "stuck".into(),
            outcome: Outcome::Succeeded,
            summary: "green".into(),
            files_changed: vec![],
            file_changes: Vec::new(),
            validation_notes: None,
            critic_verdict: None,
            gate_votes: Vec::new(),
            trace_path: None,
            diff_findings: Vec::new(),
            session_findings: Vec::new(),
            remediation: None,
            diagnostics: serde_json::json!({}),
        })
    }
}

#[tokio::test]
async fn a_stuck_build_asks_the_human_instead_of_dying_silently() {
    // The live test's actual failure: the coder built a working CLI, hit a gate it had no way to
    // satisfy, could not make further progress, and the backend returned Err(NoChanges) -- which
    // bypassed the ask entirely and killed the session. The more stuck the pack got, the less
    // able it was to ask for help. A stuck build must ask.
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let backend = Arc::new(StuckBackend { seen: seen.clone() });
    let provider = Arc::new(MockProvider::with_script("mock", vec![]));
    let pack = CodingSessionPack::with_backend(backend, provider, std::env::temp_dir());

    let (ev_tx, mut ev_rx) = mpsc::channel::<SessionEvent>(64);
    let (in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
    in_tx
        .try_send(HumanInput::new("the release token is ORCHID-7Q"))
        .unwrap();
    drop(in_tx);
    let inputs = InputChannel::new(in_rx, None);
    let (_c_tx, cancel) = tokio::sync::watch::channel(false);

    let workspace = std::env::temp_dir().join("liberado-e5-stuck-test");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut g = goal("make a todo cli");
    g.payload = serde_json::json!({
        "workspace_root": workspace.to_string_lossy(),
        "intake": { "enabled": false },
        // Mock backend — no real durable worktree needed (avoids shared s1 collisions).
        "force_host_local": true,
    });

    let store = Arc::new(liberado_session::GoalSessionStore::new());
    let mut spec = g.clone();
    spec.id = Some("s1".into());
    liberado_session::SessionRecordStore::insert(
        store.as_ref(),
        liberado_session::GoalSessionRecord::new(spec),
    )
    .await;
    let grant = liberado_session::SessionGrant {
        capabilities: [Capability::AskHuman].into_iter().collect(),
        ..Default::default()
    };
    let ctx = PackContext::new(&grant, store.clone(), "s1");

    let out = pack
        .run("s1", &g, &ctx, ev_tx, inputs, cancel)
        .await
        .unwrap();

    assert_eq!(
        prompts(&mut ev_rx).len(),
        1,
        "a stuck backend must ask the human, not die silently"
    );
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "and then actually retry with the answer");
    assert!(
        requests[1]
            .prior_feedback
            .iter()
            .any(|f| f.contains("ORCHID-7Q")),
        "the answer must reach the backend: {:#?}",
        requests[1].prior_feedback
    );
    assert_eq!(out.terminal, TerminalKind::Succeeded);
}

#[tokio::test]
async fn a_broken_environment_fails_fast_instead_of_paging_you() {
    // The other half of the distinction: no answer you could type fixes a dead sandbox. Asking
    // would be noise, and the ask is only valuable because it is rare.
    struct BrokenBackend;
    #[async_trait]
    impl CoderBackend for BrokenBackend {
        fn name(&self) -> &str {
            "broken"
        }
        async fn run(&self, _r: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
            Err(CoderError::Sandbox("workspace root vanished".into()))
        }
    }
    let provider = Arc::new(MockProvider::with_script("mock", vec![]));
    let pack =
        CodingSessionPack::with_backend(Arc::new(BrokenBackend), provider, std::env::temp_dir());

    let (ev_tx, mut ev_rx) = mpsc::channel::<SessionEvent>(64);
    let (_in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
    let inputs = InputChannel::new(in_rx, None);
    let (_c_tx, cancel) = tokio::sync::watch::channel(false);

    let workspace = std::env::temp_dir().join("liberado-e5-broken-test");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut g = goal("make a todo cli");
    g.payload = serde_json::json!({
        "workspace_root": workspace.to_string_lossy(),
        "intake": { "enabled": false },
        "force_host_local": true,
    });

    let store = Arc::new(liberado_session::GoalSessionStore::new());
    let mut spec = g.clone();
    spec.id = Some("s1".into());
    liberado_session::SessionRecordStore::insert(
        store.as_ref(),
        liberado_session::GoalSessionRecord::new(spec),
    )
    .await;
    let grant = liberado_session::SessionGrant {
        capabilities: [Capability::AskHuman].into_iter().collect(),
        ..Default::default()
    };
    let ctx = PackContext::new(&grant, store.clone(), "s1");

    let out = pack
        .run("s1", &g, &ctx, ev_tx, inputs, cancel)
        .await
        .unwrap();
    assert_eq!(out.terminal, TerminalKind::Failed);
    assert_eq!(
        prompts(&mut ev_rx).len(),
        0,
        "a dead sandbox is not a question for a human"
    );
}

#[tokio::test]
async fn the_ask_budget_bounds_the_retries_so_a_stuck_pack_cannot_interrogate_you() {
    // A pack that may ask whenever it is stuck is worse than one that guesses: it would keep
    // coming back forever. One ask (the default) means one guided retry, then it stops and
    // reports — it does not ask a second time.
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let backend = Arc::new(ScriptedBackend {
        seen: seen.clone(),
        fail_attempts: 99, // never succeeds, however much guidance it is given
    });
    let provider = Arc::new(MockProvider::with_script("mock", vec![]));
    let pack = CodingSessionPack::with_backend(backend, provider, std::env::temp_dir());

    let (ev_tx, mut ev_rx) = mpsc::channel::<SessionEvent>(64);
    let (in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
    for answer in ["try again", "and again", "and again"] {
        in_tx.try_send(HumanInput::new(answer)).unwrap();
    }
    drop(in_tx);
    let inputs = InputChannel::new(in_rx, None);
    let (_c_tx, cancel) = tokio::sync::watch::channel(false);

    let workspace = std::env::temp_dir().join("liberado-e5-budget-test");
    std::fs::create_dir_all(&workspace).unwrap();

    let mut g = goal("make a todo cli");
    g.payload = serde_json::json!({
        "workspace_root": workspace.to_string_lossy(),
        "intake": { "enabled": false },
        "force_host_local": true,
    });

    let store = Arc::new(liberado_session::GoalSessionStore::new());
    let mut spec = g.clone();
    spec.id = Some("s1".into());
    liberado_session::SessionRecordStore::insert(
        store.as_ref(),
        liberado_session::GoalSessionRecord::new(spec),
    )
    .await;
    let grant = liberado_session::SessionGrant {
        capabilities: [Capability::AskHuman].into_iter().collect(),
        ..Default::default()
    };
    let ctx = PackContext::new(&grant, store.clone(), "s1");

    let out = pack
        .run("s1", &g, &ctx, ev_tx, inputs, cancel)
        .await
        .unwrap();

    assert_eq!(
        seen.lock().unwrap().len(),
        2,
        "one ask means the initial attempt plus exactly one guided retry — no more"
    );
    assert_eq!(out.terminal, TerminalKind::Failed);
    assert_eq!(
        prompts(&mut ev_rx).len(),
        1,
        "the human is asked once, not once per failure"
    );
}

fn prompts(rx: &mut mpsc::Receiver<SessionEvent>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let SessionEventKind::AwaitingInput { prompt, .. } = ev.kind {
            out.push(prompt);
        }
    }
    out
}

#[tokio::test]
async fn intake_clarifies_then_freezes_on_accept() {
    // The S7 spine: the model asks, the human answers, the model drafts, the human accepts, and
    // the draft becomes an authoritative contract — all before a line of code is written.
    let (pack, ev_tx, mut ev_rx, mut inputs, mut cancel, _cancel_tx) = harness(
        vec![CLARIFY_JSON, &ready_json("Build a todo CLI")],
        vec!["Rust", "accept"],
    );

    let tr = Transcript::open().await;
    let ctx = tr.ctx();
    let phase = pack
        .run_intake_phase(
            "s1",
            &goal("make a todo cli"),
            &ctx,
            &settings(3),
            &ev_tx,
            &mut inputs,
            &mut cancel,
        )
        .await
        .unwrap();

    let contract = match phase {
        IntakePhase::Frozen(c) => c,
        other => panic!("expected a frozen contract, got {other:?}"),
    };
    assert_eq!(contract.draft.description, "Build a todo CLI");
    assert_eq!(contract.frozen_by, FreezeAuthority::Human);
    assert!(
        !contract.draft.verifiers.is_empty(),
        "the contract must carry the machine gates — that is the point of freezing it"
    );
    assert!(!contract.content_hash.is_empty());

    // ...and the negotiation that produced it is a **conversation**, recorded as turns.
    //
    // This used to be events only, which meant the intake Q&A was invisible to `chat-search`
    // (it matches message nodes) and the session could not be forked (forking copies a node
    // prefix, and an event log has no `parent_id`). Every question the pack asked is here.
    let turns = tr.store.turns("s1").await;
    assert!(
        turns
            .iter()
            .any(|(who, what)| *who == TurnAuthor::Assistant && what.contains("Rust or Node?")),
        "the clarifying question must be in the transcript, not just on the event bus: {turns:#?}"
    );
    assert!(
        turns
            .iter()
            .any(|(who, what)| *who == TurnAuthor::Assistant && what.contains("Build a todo CLI")),
        "so must the draft contract the human was asked to accept: {turns:#?}"
    );

    // The human saw the question (with its `affects`), then the draft for review.
    let seen = prompts(&mut ev_rx);
    assert_eq!(seen.len(), 2, "one clarify prompt + one freeze prompt");
    assert!(seen[0].contains("Rust or Node?") && seen[0].contains("verify profile"));
    assert!(seen[1].contains("Draft contract") && seen[1].contains("src/main.rs"));
}

#[tokio::test]
async fn rejecting_the_draft_builds_nothing() {
    let (pack, ev_tx, _ev_rx, mut inputs, mut cancel, _cancel_tx) =
        harness(vec![&ready_json("Build a todo CLI")], vec!["reject"]);

    let tr = Transcript::open().await;
    let ctx = tr.ctx();
    let phase = pack
        .run_intake_phase(
            "s1",
            &goal("make a todo cli"),
            &ctx,
            &settings(3),
            &ev_tx,
            &mut inputs,
            &mut cancel,
        )
        .await
        .unwrap();
    assert!(matches!(phase, IntakePhase::Rejected));
}

#[tokio::test]
async fn free_text_is_a_revision_not_an_accept() {
    // The trap this guards: "add a test for the parser" starts with 'a'. Prefix-matching it as
    // "accept" would freeze a contract the human was in the middle of changing. It must feed
    // back into intake as another answer, producing a fresh draft to review.
    let (pack, ev_tx, mut ev_rx, mut inputs, mut cancel, _cancel_tx) = harness(
        vec![&ready_json("v1"), &ready_json("v2 (revised)")],
        vec!["add a test for the parser", "accept"],
    );

    let tr = Transcript::open().await;
    let ctx = tr.ctx();
    let phase = pack
        .run_intake_phase(
            "s1",
            &goal("make a todo cli"),
            &ctx,
            &settings(3),
            &ev_tx,
            &mut inputs,
            &mut cancel,
        )
        .await
        .unwrap();

    match phase {
        IntakePhase::Frozen(c) => assert_eq!(
            c.draft.description, "v2 (revised)",
            "the revision must produce a second draft, not freeze the first"
        ),
        other => panic!("expected the revised contract to freeze, got {other:?}"),
    }
    assert_eq!(
        prompts(&mut ev_rx).len(),
        2,
        "the human reviewed two drafts"
    );
}

#[tokio::test]
async fn exhausting_clarify_rounds_stops_and_hands_back_the_partial_draft() {
    // Bounded, not an open-ended therapist loop (verifiers.md §3.4 step 5): a model that keeps
    // asking gets cut off, and the human is handed whatever was worked out rather than nothing.
    let (pack, ev_tx, _ev_rx, mut inputs, mut cancel, _cancel_tx) =
        harness(vec![CLARIFY_JSON, CLARIFY_JSON], vec!["Rust"]);

    let tr = Transcript::open().await;
    let ctx = tr.ctx();
    let phase = pack
        .run_intake_phase(
            "s1",
            &goal("something vague"),
            &ctx,
            &settings(1),
            &ev_tx,
            &mut inputs,
            &mut cancel,
        )
        .await
        .unwrap();
    assert!(
        matches!(phase, IntakePhase::NeedsReview(_)),
        "expected NeedsReview once the round budget ran out, got {phase:?}"
    );
}

#[test]
fn payload_intake_settings_beat_the_profile_overrides() {
    // A profile sets the default posture; a single session may deviate from it.
    let overrides = serde_json::json!({ "intake": { "enabled": true, "max_clarify_rounds": 3 } });
    let payload = serde_json::json!({ "intake": { "enabled": false } });
    let s = IntakeSettings::resolve(&overrides, &payload);
    assert!(!s.enabled, "payload wins over the profile");
    assert_eq!(
        s.max_clarify_rounds, 3,
        "keys the payload didn't set still fall back to the profile"
    );

    // Defaults: intake on, 3 rounds — intake-first is the whole point of a coding session.
    let d = IntakeSettings::resolve(&serde_json::json!({}), &serde_json::json!({}));
    assert!(d.enabled);
    assert_eq!(d.max_clarify_rounds, 3);
}

#[test]
fn the_draft_review_shows_what_it_will_be_judged_against() {
    let draft = GoalContractDraft {
        description: "Build a todo CLI".into(),
        success_criteria: vec!["add and list work".into()],
        verifiers: vec![
            VerifierSpec::PathsExist {
                id: "paths".into(),
                paths: vec!["src/main.rs".into()],
            },
            VerifierSpec::GitNonemptyDiff { id: "diff".into() },
        ],
        out_of_scope: vec!["network".into()],
        assumed_defaults: vec!["Rust".into()],
        domain_hint: None,
        verify_profile: None,
    };
    let out = render_draft(&draft, "the stack is clear");
    assert!(out.contains("Build a todo CLI"));
    assert!(out.contains("add and list work"));
    assert!(
        out.contains("src/main.rs"),
        "the machine gates must be visible before freeze, not after"
    );
    assert!(out.contains("must actually change"));
    assert!(out.contains("network") && out.contains("Rust"));
    assert!(out.contains("the stack is clear"));
    assert!(out.contains("accept") && out.contains("reject"));
}

/// The incoherent draft from the live run, reproduced: `verify_profile = "rust-strict"` injects
/// clippy/fmt while the model's own `out_of_scope` sincerely says it dropped them. This must
/// never reach the human — it goes straight back to the model, and the human is only ever shown
/// the coherent redraft.
#[tokio::test]
async fn a_self_contradicting_draft_goes_back_to_the_model_not_to_the_human() {
    let incoherent = serde_json::to_string(&IntakeOutcome::ReadyForFreeze {
        draft: GoalContractDraft {
            description: "Build a todo CLI".into(),
            success_criteria: vec!["it works".into()],
            verifiers: vec![],
            out_of_scope: vec!["No clippy or fmt checks.".into()],
            assumed_defaults: vec![],
            domain_hint: Some("coding".into()),
            // The trap: this silently re-adds cargo-clippy and cargo-fmt at expansion time, so
            // the prose above becomes a lie about a list the model never sees.
            verify_profile: Some("rust-strict".into()),
        },
        rationale: "ready".into(),
    })
    .unwrap();

    let (pack, ev_tx, mut ev_rx, mut inputs, mut cancel, _c) = harness(
        vec![&incoherent, &ready_json("Build a todo CLI")],
        vec!["accept"],
    );

    let tr = Transcript::open().await;
    let ctx = tr.ctx();
    let phase = pack
        .run_intake_phase(
            "s1",
            &goal("make a todo cli"),
            &ctx,
            &settings(3),
            &ev_tx,
            &mut inputs,
            &mut cancel,
        )
        .await
        .unwrap();

    assert!(
        matches!(phase, IntakePhase::Frozen(_)),
        "the redraft should freeze: {phase:?}"
    );

    // The human was asked exactly ONCE — for the coherent redraft. They never saw the
    // contradictory one, because catching it is the machine's job, not theirs.
    let seen = prompts(&mut ev_rx);
    assert_eq!(
        seen.len(),
        1,
        "the human must not be shown a draft that contradicts itself: {seen:#?}"
    );
    assert!(
        !seen[0].contains("No clippy or fmt checks"),
        "and certainly not the contradictory one: {}",
        seen[0]
    );
}

#[tokio::test]
async fn freeze_refuses_a_contract_that_contradicts_itself() {
    // Belt and braces: even if a contradictory draft somehow reaches freeze, freeze refuses.
    // Freezing is what makes the gates binding — the worker cannot argue with them — so binding
    // it to something impossible means it obeys, faithfully, into the ground.
    let mut draft = GoalContractDraft {
        description: "Build a todo CLI".into(),
        success_criteria: vec!["it works".into()],
        verifiers: vec![],
        out_of_scope: vec!["No clippy checks.".into()],
        assumed_defaults: vec![],
        domain_hint: None,
        verify_profile: Some("rust-strict".into()),
    };
    liberado_coder_core::sanitize_draft(&mut draft);

    let err = GoalContract::freeze("s1", draft, FreezeAuthority::Human)
        .expect_err("a self-contradictory contract must not become binding");
    assert!(err.contains("contradicts itself"), "{err}");
    assert!(err.contains("cargo-clippy"), "must name the gate: {err}");
}

#[test]
fn the_transcript_rebuilds_the_intake_answers() {
    // The shape a real parked coding session leaves behind. The FIRST user turn is the goal --
    // not an answer to anything -- and getting that wrong would feed the goal back to the model
    // as though it were a reply, which is exactly the kind of off-by-one that produces a
    // confidently wrong second question.
    let turns = vec![
        (TurnAuthor::User, "make a todo cli".to_string()),
        (TurnAuthor::Assistant, "Rust or Node?".to_string()),
        (TurnAuthor::User, "Rust".to_string()),
        (
            TurnAuthor::Assistant,
            "What file path for persistence?".to_string(),
        ),
        (TurnAuthor::User, "todos.json".to_string()),
    ];
    let answers = answers_from_transcript(&turns);
    assert_eq!(answers.len(), 2, "the goal is not an answer: {answers:#?}");
    assert_eq!(answers[0].question_id, "Rust or Node?");
    assert_eq!(answers[0].answer, "Rust");
    assert_eq!(answers[1].question_id, "What file path for persistence?");
    assert_eq!(answers[1].answer, "todos.json");
}

#[test]
fn a_fresh_session_reconstructs_nothing() {
    // The normal case must cost nothing and, above all, must not invent an answer.
    assert!(answers_from_transcript(&[]).is_empty());
    assert!(
        answers_from_transcript(&[(TurnAuthor::User, "make a todo cli".into())]).is_empty(),
        "a session that has only been given its goal has answered nothing"
    );
}

#[tokio::test]
async fn the_coding_pack_will_not_resume_once_the_build_has_started() {
    // The line where irreversibility begins. Intake touches nothing, so an approximate
    // reconstruction is safe -- it ends at a draft a human must accept. The build EDITS FILES,
    // and re-running it from an approximate reconstruction, against a workspace no longer in the
    // state that reconstruction assumes, is how you quietly corrupt someone's work.
    let provider = Arc::new(MockProvider::with_script("mock", vec![]));
    let pack = CodingSessionPack::new(provider, std::env::temp_dir());
    let store = Arc::new(liberado_session::GoalSessionStore::new());
    let mut spec = goal("make a todo cli");
    spec.id = Some("s1".into());
    liberado_session::SessionRecordStore::insert(
        store.as_ref(),
        liberado_session::GoalSessionRecord::new(spec),
    )
    .await;
    let grant = liberado_session::SessionGrant::default();

    let ctx = PackContext::new(&grant, store.clone(), "s1");
    assert!(
        pack.can_resume(&ctx).await,
        "a session still in intake is resumable"
    );

    // The build starts.
    liberado_session::SessionRecordStore::push_event(
        store.as_ref(),
        SessionEvent::new(
            "s1",
            SessionEventKind::RoleStarted {
                role: "coder".into(),
                model: "m".into(),
            },
        ),
    )
    .await;

    assert!(
        !pack.can_resume(&ctx).await,
        "once the build has started without a checkpoint, resume is refused"
    );

    // A checkpoint event makes mid-build resume safe (S4 / E6-c(b)).
    liberado_session::SessionRecordStore::push_event(
        store.as_ref(),
        SessionEvent::new(
            "s1",
            SessionEventKind::Checkpoint {
                id: "abc123".into(),
                label: "attempt-0-post".into(),
                tree_hash: "tree1".into(),
            },
        ),
    )
    .await;
    assert!(
        pack.can_resume(&ctx).await,
        "mid-build resume is allowed once a workspace checkpoint exists"
    );
}

#[tokio::test]
async fn ship_preflight_failure_blocks_terminal_succeeded() {
    // Build "succeeds" but ship preflight is required and its step fails → Failed, not Succeeded.
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let backend = Arc::new(ScriptedBackend {
        seen: seen.clone(),
        fail_attempts: 0,
    });
    let provider = Arc::new(MockProvider::with_script("mock", vec![]));
    let pack = CodingSessionPack::with_backend(backend, provider, std::env::temp_dir());

    let (ev_tx, mut ev_rx) = mpsc::channel::<SessionEvent>(64);
    let (_in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
    let inputs = InputChannel::new(in_rx, None);
    let (_c_tx, cancel) = tokio::sync::watch::channel(false);

    let workspace = tempfile::tempdir().unwrap();
    let fail = if cfg!(windows) { "exit /B 1" } else { "exit 1" };
    let mut g = goal("ship me");
    g.payload = serde_json::json!({
        "workspace_root": workspace.path().to_string_lossy(),
        "intake": { "enabled": false },
        "force_host_local": true,
        "preflight": {
            "required": true,
            "steps": [
                { "name": "must-fail", "run": fail }
            ]
        }
    });

    let store = Arc::new(liberado_session::GoalSessionStore::new());
    let mut spec = g.clone();
    spec.id = Some("pf1".into());
    liberado_session::SessionRecordStore::insert(
        store.as_ref(),
        liberado_session::GoalSessionRecord::new(spec),
    )
    .await;
    let grant = liberado_session::SessionGrant::default();
    let ctx = PackContext::new(&grant, store.clone(), "pf1");

    let out = pack
        .run("pf1", &g, &ctx, ev_tx, inputs, cancel)
        .await
        .unwrap();
    assert_eq!(
        out.terminal,
        TerminalKind::Failed,
        "failed ship preflight must not Succeeded: {out:?}"
    );
    assert!(
        out.summary.contains("preflight") || out.summary.contains("must-fail"),
        "summary should mention preflight: {}",
        out.summary
    );
    // Surface evidence: ValidationFinished with ok=false for preflight
    let mut saw_preflight_validation = false;
    while let Ok(ev) = ev_rx.try_recv() {
        if let SessionEventKind::ValidationFinished { ok: false, summary } = ev.kind
            && (summary.contains("preflight") || summary.contains("must-fail"))
        {
            saw_preflight_validation = true;
        }
    }
    assert!(
        saw_preflight_validation,
        "preflight failure must emit validation_finished"
    );
}

#[tokio::test]
async fn ship_preflight_green_allows_succeeded() {
    let backend = Arc::new(ScriptedBackend {
        seen: Arc::new(std::sync::Mutex::new(Vec::new())),
        fail_attempts: 0,
    });
    let provider = Arc::new(MockProvider::with_script("mock", vec![]));
    let pack = CodingSessionPack::with_backend(backend, provider, std::env::temp_dir());
    let (ev_tx, _ev_rx) = mpsc::channel::<SessionEvent>(64);
    let (_in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
    let inputs = InputChannel::new(in_rx, None);
    let (_c_tx, cancel) = tokio::sync::watch::channel(false);
    let workspace = tempfile::tempdir().unwrap();
    let mut g = goal("ship me green");
    g.payload = serde_json::json!({
        "workspace_root": workspace.path().to_string_lossy(),
        "intake": { "enabled": false },
        "force_host_local": true,
        "preflight": {
            "required": true,
            "steps": [ { "name": "ok", "run": "echo green" } ]
        }
    });
    let store = Arc::new(liberado_session::GoalSessionStore::new());
    let mut spec = g.clone();
    spec.id = Some("pf2".into());
    liberado_session::SessionRecordStore::insert(
        store.as_ref(),
        liberado_session::GoalSessionRecord::new(spec),
    )
    .await;
    let grant = liberado_session::SessionGrant::default();
    let ctx = PackContext::new(&grant, store.clone(), "pf2");
    let out = pack
        .run("pf2", &g, &ctx, ev_tx, inputs, cancel)
        .await
        .unwrap();
    assert_eq!(out.terminal, TerminalKind::Succeeded, "{out:?}");
    assert!(
        out.diagnostics.get("preflight").is_some(),
        "diagnostics should include preflight report: {}",
        out.diagnostics
    );
}

#[tokio::test]
async fn an_external_workspace_gets_durable_session_isolation() {
    use std::process::Command;

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let backend = Arc::new(ScriptedBackend {
        seen: seen.clone(),
        fail_attempts: 0,
    });
    let provider = Arc::new(MockProvider::with_script("mock", vec![]));
    let pack = CodingSessionPack::with_backend(backend, provider, std::env::temp_dir());

    let (ev_tx, _ev_rx) = mpsc::channel::<SessionEvent>(64);
    let (_in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
    let inputs = InputChannel::new(in_rx, None);
    let (_c_tx, cancel) = tokio::sync::watch::channel(false);

    let workspace = tempfile::tempdir().unwrap();
    let workspace_root = workspace.path().to_path_buf();
    let data = tempfile::tempdir().unwrap();
    // `LIBERADO_DATA_DIR` is process-global and the fanout tests set it too; hold the guard for
    // as long as this test depends on the value.
    let _env = crate::DATA_DIR_ENV_LOCK.lock().await;
    // SAFETY: test-only env mutation, serialized by the guard above; restored below.
    unsafe {
        std::env::set_var("LIBERADO_DATA_DIR", data.path());
    }

    let mut g = goal("edit README.md");
    g.payload = serde_json::json!({
        "workspace_root": workspace_root.to_string_lossy(),
        "intake": { "enabled": false },
    });

    let store = Arc::new(liberado_session::GoalSessionStore::new());
    let mut spec = g.clone();
    spec.id = Some("s1".into());
    liberado_session::SessionRecordStore::insert(
        store.as_ref(),
        liberado_session::GoalSessionRecord::new(spec),
    )
    .await;
    let grant = liberado_session::SessionGrant::default();
    let ctx = PackContext::new(&grant, store.clone(), "s1");

    let _out = pack
        .run("s1", &g, &ctx, ev_tx, inputs, cancel)
        .await
        .unwrap();

    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].config.sandbox,
        SandboxSpec::HostLocal,
        "durable session worktree is operated as HostLocal (survives park Drop)"
    );
    let attempt_root = PathBuf::from(&requests[0].workspace.root);
    assert!(
        attempt_root.ends_with("s1") || attempt_root.file_name().is_some_and(|n| n == "s1"),
        "attempt workspace should be coding-worktrees/s1, got {}",
        attempt_root.display()
    );
    assert!(
        attempt_root.exists(),
        "durable session worktree must remain after the attempt: {}",
        attempt_root.display()
    );

    // Parent is a real git repo with a commit (seed for the linked worktree).
    let output = Command::new("git")
        .args(["-C", &workspace_root.to_string_lossy()])
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "init_git_repo must run so session worktree can proceed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    unsafe {
        std::env::remove_var("LIBERADO_DATA_DIR");
    }
}

/// `[tuning.coder.gate]` reaches the `CoderRunConfig` the pack actually hands the backend.
///
/// Every run config this pack built previously hardcoded `CoderGateConfig::default()`, so the gate
/// — 1,767 lines of gatekeeper plus cold-reviewer quorum, and a documented config table — could not
/// be switched on through the daemon at any setting. The backend already honoured
/// `request.config.gate.enabled`; only the wire between them was missing.
///
/// Asserted against the recorded request rather than the builder, because the builder was never
/// the broken part: a test that only reads `pack.gate` passes with the hardcoded default still in
/// place, which is exactly what the first version of this test did.
#[tokio::test]
async fn the_configured_gate_reaches_the_backends_run_config() {
    use liberado_coder_core::CoderGateConfig;

    async fn gate_seen_by_backend(configure: bool) -> CoderGateConfig {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let backend = Arc::new(ScriptedBackend {
            seen: seen.clone(),
            fail_attempts: 0,
        });
        let provider = Arc::new(MockProvider::with_script("mock", vec![]));
        let mut pack = CodingSessionPack::with_backend(backend, provider, std::env::temp_dir());
        if configure {
            pack = pack.with_gate(CoderGateConfig {
                enabled: true,
                fresh_reviewers: 3,
                ..CoderGateConfig::default()
            });
        }

        let (ev_tx, _ev_rx) = mpsc::channel::<SessionEvent>(64);
        let (in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
        drop(in_tx);
        let inputs = InputChannel::new(in_rx, None);
        let (_c_tx, cancel) = tokio::sync::watch::channel(false);

        let workspace = std::env::temp_dir().join(format!(
            "liberado-gate-wire-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        let mut g = goal("wire check");
        g.payload = serde_json::json!({
            "workspace_root": workspace.to_string_lossy(),
            "intake": { "enabled": false },
            "force_host_local": true,
            "skip_preflight": true,
        });

        let store = Arc::new(liberado_session::GoalSessionStore::new());
        let mut spec = g.clone();
        spec.id = Some("gate1".into());
        liberado_session::SessionRecordStore::insert(
            store.as_ref(),
            liberado_session::GoalSessionRecord::new(spec),
        )
        .await;
        let grant = liberado_session::SessionGrant::default();
        let ctx = PackContext::new(&grant, store.clone(), "gate1");

        let _ = pack.run("gate1", &g, &ctx, ev_tx, inputs, cancel).await;
        let requests = seen.lock().unwrap().clone();
        let _ = std::fs::remove_dir_all(&workspace);
        assert!(!requests.is_empty(), "backend was never invoked");
        requests[0].config.gate.clone()
    }

    // Unconfigured stays off — this makes the gate reachable, not enabled. It costs
    // `1 + fresh_reviewers` extra model calls per attempt, so switching it on is the operator's.
    assert!(!gate_seen_by_backend(false).await.enabled);

    let configured = gate_seen_by_backend(true).await;
    assert!(
        configured.enabled,
        "a configured gate must reach the backend, not be replaced by the default"
    );
    assert_eq!(
        configured.fresh_reviewers, 3,
        "the whole config must survive, not just the enabled flag"
    );
}

/// `[tuning.coder.progress]` reaches the `CoderRunConfig` the pack hands the backend.
///
/// `CoderTuning` has carried a validated `progress` table since the guard was written, and this
/// build path hardcoded `ProgressPolicy::default()`, so the one set of thresholds an operator most
/// needs to adjust per repo could only be changed by editing Rust and recompiling. The limits
/// govern how many inspect calls a task may spend before the guard declares a stall — which depends
/// entirely on how many files the change spans, i.e. on the repo, not on the code.
///
/// Asserted against the recorded request, not the builder, for the reason the gate test gives.
#[tokio::test]
async fn the_configured_progress_policy_reaches_the_backends_run_config() {
    use liberado_coder_core::ProgressPolicy;

    async fn progress_seen_by_backend(configure: bool) -> ProgressPolicy {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let backend = Arc::new(ScriptedBackend {
            seen: seen.clone(),
            fail_attempts: 0,
        });
        let provider = Arc::new(MockProvider::with_script("mock", vec![]));
        let mut pack = CodingSessionPack::with_backend(backend, provider, std::env::temp_dir());
        if configure {
            pack = pack.with_progress(ProgressPolicy {
                read_only_turn_limit: 37,
                same_tool_limit: 19,
                ..ProgressPolicy::default()
            });
        }

        let (ev_tx, _ev_rx) = mpsc::channel::<SessionEvent>(64);
        let (in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
        drop(in_tx);
        let inputs = InputChannel::new(in_rx, None);
        let (_c_tx, cancel) = tokio::sync::watch::channel(false);

        let workspace = std::env::temp_dir().join(format!(
            "liberado-progress-wire-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        let mut g = goal("wire check");
        g.payload = serde_json::json!({
            "workspace_root": workspace.to_string_lossy(),
            "intake": { "enabled": false },
            "force_host_local": true,
            "skip_preflight": true,
        });

        let store = Arc::new(liberado_session::GoalSessionStore::new());
        let mut spec = g.clone();
        spec.id = Some("prog1".into());
        liberado_session::SessionRecordStore::insert(
            store.as_ref(),
            liberado_session::GoalSessionRecord::new(spec),
        )
        .await;
        let grant = liberado_session::SessionGrant::default();
        let ctx = PackContext::new(&grant, store.clone(), "prog1");

        let _ = pack.run("prog1", &g, &ctx, ev_tx, inputs, cancel).await;
        let requests = seen.lock().unwrap().clone();
        let _ = std::fs::remove_dir_all(&workspace);
        assert!(!requests.is_empty(), "backend was never invoked");
        requests[0].config.progress.clone()
    }

    // Unconfigured keeps the code-owned defaults, so this changes reachability, not behaviour.
    let unconfigured = progress_seen_by_backend(false).await;
    assert_eq!(
        unconfigured.read_only_turn_limit,
        ProgressPolicy::default().read_only_turn_limit
    );

    let configured = progress_seen_by_backend(true).await;
    assert_eq!(
        configured.read_only_turn_limit, 37,
        "a configured read_only_turn_limit must reach the backend, not be replaced by the default"
    );
    assert_eq!(
        configured.same_tool_limit, 19,
        "the whole table must survive, not just one field"
    );
}

/// The configured `[coder.coder]` model and turn ceiling reach the run the backend is handed.
///
/// The pack previously passed the literal `"session-coder"` — a name no provider resolves — and
/// its own 12-turn constant, so an operator reading `deepseek/deepseek-v4-pro` and `max_turns = 30`
/// in tuning.toml got neither. It went unnoticed because `SingleProviderFactory` ignored the model
/// anyway, which made the wrong string harmless right up until the factory started honouring it.
#[tokio::test]
async fn the_configured_coder_role_reaches_the_backends_run_config() {
    use liberado_coder_core::CoderRoleConfig;

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let backend = Arc::new(ScriptedBackend {
        seen: seen.clone(),
        fail_attempts: 0,
    });
    let provider = Arc::new(MockProvider::with_script("mock", vec![]));
    let pack = CodingSessionPack::with_backend(backend, provider, std::env::temp_dir())
        .with_coder_role(CoderRoleConfig {
            model: "deepseek/deepseek-v4-pro".into(),
            prompt_path: None,
            prompt: None,
            temperature: None,
            max_tokens: None,
            max_turns: Some(30),
        });

    let (ev_tx, _ev_rx) = mpsc::channel::<SessionEvent>(64);
    let (in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
    drop(in_tx);
    let inputs = InputChannel::new(in_rx, None);
    let (_c_tx, cancel) = tokio::sync::watch::channel(false);

    let workspace = std::env::temp_dir().join(format!(
        "liberado-role-wire-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&workspace).unwrap();

    let mut g = goal("role check");
    g.payload = serde_json::json!({
        "workspace_root": workspace.to_string_lossy(),
        "intake": { "enabled": false },
        "force_host_local": true,
        "skip_preflight": true,
    });

    let store = Arc::new(liberado_session::GoalSessionStore::new());
    let mut spec = g.clone();
    spec.id = Some("role1".into());
    liberado_session::SessionRecordStore::insert(
        store.as_ref(),
        liberado_session::GoalSessionRecord::new(spec),
    )
    .await;
    let grant = liberado_session::SessionGrant::default();
    let ctx = PackContext::new(&grant, store.clone(), "role1");

    let _ = pack.run("role1", &g, &ctx, ev_tx, inputs, cancel).await;
    let requests = seen.lock().unwrap().clone();
    let _ = std::fs::remove_dir_all(&workspace);

    assert!(!requests.is_empty(), "backend was never invoked");
    let coder = &requests[0].config.coder;
    assert_eq!(
        coder.model, "deepseek/deepseek-v4-pro",
        "the configured model must reach the run, not the `session-coder` placeholder"
    );
    assert_eq!(
        coder.max_turns,
        Some(30),
        "the configured ceiling must reach the run, not the pack's 12-turn constant"
    );
}
