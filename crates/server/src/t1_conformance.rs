//! Tier-1 live conformance suite (docs/roadmap/live-conformance-suite.md).
//!
//! In-process, production-shaped goals HTTP surface over a **durable** `SessionStore`
//! (the same store type production boots) + `MockProvider` where a model is required.
//! No network, no API key, no real vault — CI-safe.
//!
//! Landed checks: **L1** (ask → answer → pack continued with answer), **L2** (durable reopen),
//! **L3** (parked resume + prior turns), **L4** (irreversible pack refuses resume), **L5**
//! (AskHuman 403), **L6** (Write both-arms), **L7** (alert dual-arm), **L8** (cancel → Cancelled),
//! **L9** (cron/webhook → joinable Dispatched session — see `liberado-daemon` `l9_*` tests),
//! **L10** (fork prefix).

#![cfg(test)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use liberado_common::{
    Capability, CapabilitySet, Consequence, McpDescriptor, ProposalSigner, WriteClass, Zone,
};
use liberado_conversation_store::{Author, ConversationStore, NewNode, Ulid};
use liberado_executor::{RiskGatedToolRuntime, ToolRuntime};
use liberado_provider::{Message, MockProvider, ToolDef, ToolInvocation};
use liberado_session::{
    DomainHint, DomainPackRunner, GoalResult, GoalSessionHub, GoalSpec, InputOutcome,
    LifeOpsDemoRunner, PackContext, PackError, SessionAlert, SessionEvent, SessionEventKind,
    SessionGrant, SessionSnapshot, SessionStatus, TerminalKind, TurnAuthor,
};
use liberado_session_store::{NewSession, SessionStore};
use tower::ServiceExt;

use crate::api::{goals_cancel, goals_message, goals_start, session_fork};
use crate::state::AppState;

// ── Harness ─────────────────────────────────────────────────────────────────

/// Temp-isolated, production-shaped surface: durable session store + goal hub + goal/session routes.
struct T1Harness {
    app: Router,
    goals: Arc<GoalSessionHub>,
    sessions: Arc<SessionStore>,
    /// Durable store root (also keeps the temp dir alive for the test lifetime).
    #[allow(dead_code)]
    root: PathBuf,
}

impl T1Harness {
    /// Life-ops pack on a durable store (production wiring: hub over `SessionStore`).
    async fn with_life_pack() -> Self {
        Self::build(
            Arc::new(LifeOpsDemoRunner),
            life_config_with_ask_human(),
            None,
        )
        .await
    }

    /// Custom pack (e.g. never-ending for L8 cancel).
    async fn with_pack(
        pack: Arc<dyn DomainPackRunner>,
        config: liberado_bootstrap::Config,
    ) -> Self {
        Self::build(pack, config, None).await
    }

    /// Custom pack + optional `SessionAlert` (L7).
    async fn with_pack_and_alert(
        pack: Arc<dyn DomainPackRunner>,
        config: liberado_bootstrap::Config,
        alert: Arc<dyn SessionAlert>,
    ) -> Self {
        Self::build(pack, config, Some(alert)).await
    }

    /// Re-open an existing durable root (daemon-restart shape for L3/L4).
    async fn reopen_at(
        root: PathBuf,
        pack: Arc<dyn DomainPackRunner>,
        config: liberado_bootstrap::Config,
    ) -> Self {
        Self::build_at(root, pack, config, None).await
    }

    async fn build(
        pack: Arc<dyn DomainPackRunner>,
        config: liberado_bootstrap::Config,
        alert: Option<Arc<dyn SessionAlert>>,
    ) -> Self {
        let root = std::env::temp_dir().join(format!("liberado-t1-{}", Ulid::new()));
        Self::build_at(root, pack, config, alert).await
    }

    async fn build_at(
        root: PathBuf,
        pack: Arc<dyn DomainPackRunner>,
        config: liberado_bootstrap::Config,
        alert: Option<Arc<dyn SessionAlert>>,
    ) -> Self {
        let sessions = Arc::new(SessionStore::open(&root).await);

        // Production shape: hub shares the converged store (lib.rs `run`).
        let mut hub = GoalSessionHub::new(SessionStore::clone(&sessions));
        if let Some(alert) = alert {
            hub = hub.with_alert(alert);
        }
        hub.register_pack(pack);
        let goals = Arc::new(hub);

        let (hook_tx, _hook_rx) = tokio::sync::mpsc::unbounded_channel();
        // MockProvider is present so the surface matches production chat-capable boots when needed;
        // L5/L8/L10 do not require completions.
        let _provider: Arc<dyn liberado_provider::Provider> =
            Arc::new(MockProvider::with_script("t1-mock", vec![]));

        let state = Arc::new(AppState {
            start_time: Instant::now(),
            reactions: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            dispatcher_attached: false,
            orchestrator_attached: false,
            vault_path: root.join("vault").to_string_lossy().into_owned(),
            goals: goals.clone(),
            chat: None,
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            catalog: Arc::new(liberado_common::CapabilityCatalog::new()),
            sessions_root: root.clone(),
            main_agent_capabilities: CapabilitySet::empty(),
            dispatcher_capabilities: CapabilitySet::empty(),
            config: Arc::new(config),
            sessions: sessions.clone(),
            model_name: Some("t1-mock".into()),
            provider: Some(_provider),
            hooks: std::collections::HashMap::new(),
            hook_tx,
            hook_idempotency: crate::hooks::IdempotencyCache::default(),
            live_mcp: liberado_bootstrap::LiveMcpController::empty(),
        });

        let app = Router::new()
            .route(
                "/api/goals",
                axum::routing::get(crate::api::goals_list).post(goals_start),
            )
            .route("/api/goals/{id}", axum::routing::get(crate::api::goals_get))
            .route("/api/goals/{id}/cancel", axum::routing::post(goals_cancel))
            .route(
                "/api/goals/{id}/message",
                axum::routing::post(goals_message),
            )
            .route("/api/sessions/{id}/fork", axum::routing::post(session_fork))
            .with_state(state);

        Self {
            app,
            goals,
            sessions,
            root,
        }
    }

    async fn post_json(&self, uri: &str, body: &str) -> (StatusCode, String) {
        let resp = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        (
            status,
            String::from_utf8(bytes.to_vec()).unwrap_or_default(),
        )
    }
}

fn life_capabilities_with_ask_human() -> CapabilitySet {
    let mut caps = CapabilitySet::empty();
    caps.grant(Capability::AskHuman);
    caps.grant(Capability::Write(Zone::vault("tasks")));
    caps
}

fn life_config_with_ask_human() -> liberado_bootstrap::Config {
    use liberado_config::Grant;
    let mut config = liberado_bootstrap::Config::default();
    // vault_path required by Config::default may be empty — fine for these tests (not loading files).
    config.policy.grants.push(Grant {
        component: "life".into(),
        capabilities: life_capabilities_with_ask_human().capabilities,
    });
    config
}

/// Config whose `"life"` component has **no** AskHuman — unattended / zero-authority for human input.
fn life_config_without_ask_human() -> liberado_bootstrap::Config {
    use liberado_config::Grant;
    let mut config = liberado_bootstrap::Config::default();
    config.policy.grants.push(Grant {
        component: "life".into(),
        capabilities: vec![Capability::Write(Zone::vault("tasks"))],
    });
    config
}

async fn wait_status(
    goals: &Arc<GoalSessionHub>,
    id: &str,
    pred: impl Fn(&SessionSnapshot) -> bool,
) -> SessionSnapshot {
    for _ in 0..400 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        if let Some(snap) = goals.snapshot(id).await
            && pred(&snap)
        {
            return snap;
        }
    }
    panic!("session {id} never matched wait predicate");
}

// ── L7 — alert fires iff no live subscriber (dual-arm) ───────────────────────

/// Spy `SessionAlert`: ground truth for whether E5's unwatched-awaiting ping ran.
struct SpySessionAlert {
    calls: Arc<Mutex<Vec<(String, String)>>>,
}

#[async_trait]
impl SessionAlert for SpySessionAlert {
    async fn session_needs_you(&self, session_id: &str, prompt: &str) {
        self.calls
            .lock()
            .unwrap()
            .push((session_id.to_string(), prompt.to_string()));
    }
}

/// Pack that only emits `AwaitingInput` after an external release — so L7 can attach a live
/// store subscriber **before** the alert gate runs (avoids a race with a free-running pack).
struct GatedAskPack {
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl DomainPackRunner for GatedAskPack {
    fn domain_id(&self) -> &str {
        "life"
    }

    async fn run(
        &self,
        session_id: &str,
        _goal: &GoalSpec,
        ctx: &PackContext<'_>,
        events: tokio::sync::mpsc::Sender<SessionEvent>,
        mut inputs: liberado_session::InputChannel,
        mut cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        // Wait until the test has subscribed (or decided not to).
        tokio::select! {
            _ = self.release.notified() => {}
            _ = cancel.changed() => return Err(PackError::Cancelled),
        }
        if *cancel.borrow() {
            return Err(PackError::Cancelled);
        }

        const QUESTION: &str = "L7: what should I title the note?";
        ctx.record_turn(TurnAuthor::Assistant, QUESTION).await;
        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::AwaitingInput {
                    prompt: QUESTION.into(),
                    options: Vec::new(),
                },
            ))
            .await;

        // Park until cancel/answer so the session stays awaiting for the assert window.
        let outcome = tokio::select! {
            o = inputs.recv() => o,
            _ = cancel.changed() => InputOutcome::Closed,
        };
        match outcome {
            InputOutcome::Received(input) => Ok(GoalResult {
                terminal: TerminalKind::Succeeded,
                summary: format!("answered: {}", input.text),
                artifacts: vec![],
                diagnostics: serde_json::Value::Null,
            }),
            InputOutcome::IdleExpired(d) => Ok(GoalResult {
                terminal: TerminalKind::BudgetExhausted,
                summary: format!("idle {}", d.as_secs()),
                artifacts: vec![],
                diagnostics: serde_json::Value::Null,
            }),
            InputOutcome::Closed => Err(PackError::Cancelled),
        }
    }
}

/// L7 refuse-suppress arm (positive control): a **live** store subscriber means alert must **not** fire.
#[tokio::test]
async fn l7_awaiting_with_live_subscriber_does_not_alert() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let alert = Arc::new(SpySessionAlert {
        calls: calls.clone(),
    });
    let release = Arc::new(tokio::sync::Notify::new());
    let harness = T1Harness::with_pack_and_alert(
        Arc::new(GatedAskPack {
            release: release.clone(),
        }),
        life_config_with_ask_human(),
        alert,
    )
    .await;

    let (status, body) = harness
        .post_json(
            "/api/goals",
            r#"{"description":"L7 watched session","domain":"life","payload":{"interactive":true},"success_criteria":[]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Session must exist on the durable store before subscribe is meaningful.
    wait_status(&harness.goals, &id, |s| {
        s.session.status == SessionStatus::Running
    })
    .await;

    // Real production bus: subscribe increments live_subscriber_count (session-store).
    let sub = harness
        .goals
        .store()
        .subscribe(&id)
        .await
        .expect("subscribe must work on a live session");
    let (_history, _rx) = sub;
    assert!(
        harness.goals.store().live_subscriber_count(&id).await >= 1,
        "L7 positive control requires a real live subscriber on the store bus"
    );

    // Now let the pack ask — alert gate runs with subscriber_count > 0.
    release.notify_one();
    wait_status(&harness.goals, &id, |s| s.session.awaiting_input).await;

    // Give the event pump a beat to process AwaitingInput.
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        calls.lock().unwrap().is_empty(),
        "L7: live subscriber must suppress session_needs_you — got {:?}",
        calls.lock().unwrap()
    );
}

/// L7 fire arm: no live subscriber → `SessionAlert::session_needs_you` **does** fire.
#[tokio::test]
async fn l7_awaiting_without_subscriber_fires_alert() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let alert = Arc::new(SpySessionAlert {
        calls: calls.clone(),
    });
    let release = Arc::new(tokio::sync::Notify::new());
    let harness = T1Harness::with_pack_and_alert(
        Arc::new(GatedAskPack {
            release: release.clone(),
        }),
        life_config_with_ask_human(),
        alert,
    )
    .await;

    let (status, body) = harness
        .post_json(
            "/api/goals",
            r#"{"description":"L7 unwatched session","domain":"life","payload":{"interactive":true},"success_criteria":[]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_string();

    wait_status(&harness.goals, &id, |s| {
        s.session.status == SessionStatus::Running
    })
    .await;
    assert_eq!(
        harness.goals.store().live_subscriber_count(&id).await,
        0,
        "L7 fire arm: no subscriber attached"
    );

    release.notify_one();
    wait_status(&harness.goals, &id, |s| s.session.awaiting_input).await;

    // Poll until the alert path has run (pump is async).
    let mut fired = false;
    for _ in 0..100 {
        if !calls.lock().unwrap().is_empty() {
            fired = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        fired,
        "L7: unwatched AwaitingInput must call session_needs_you"
    );
    let log = calls.lock().unwrap().clone();
    assert_eq!(log.len(), 1, "exactly one alert for one ask: {log:?}");
    assert_eq!(log[0].0, id);
    assert!(
        log[0].1.contains("L7") || log[0].1.contains("title"),
        "alert should carry the pack prompt: {:?}",
        log[0].1
    );
}

// ── L1 — ask → answer over POST /message → pack continued with the answer ───

/// L1: spawn interactive session (AskHuman granted) → pack awaits input → human answers via
/// production `POST /api/goals/{id}/message` → session succeeds and **ground truth shows the pack
/// continued with that answer** (result summary / artifacts), not only that an event claimed so.
#[tokio::test]
async fn l1_ask_answer_pack_continues_with_answer_ground_truth() {
    let harness = T1Harness::with_life_pack().await;
    const ANSWER: &str = "L1-Weekly-Review-Secret";

    let (status, body) = harness
        .post_json(
            "/api/goals",
            r#"{"description":"capture a note interactively","domain":"life","payload":{"interactive":true},"success_criteria":[]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "start body: {body}");
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["session_id"]
        .as_str()
        .expect("session_id")
        .to_string();

    wait_status(&harness.goals, &id, |s| s.session.awaiting_input).await;

    let (msg_status, msg_body) = harness
        .post_json(
            &format!("/api/goals/{id}/message"),
            &format!(r#"{{"text":"{ANSWER}"}}"#),
        )
        .await;
    assert_eq!(
        msg_status,
        StatusCode::ACCEPTED,
        "L1 live send_input path: {msg_body}"
    );

    let snap = wait_status(&harness.goals, &id, |s| s.session.status.is_terminal()).await;
    assert_eq!(
        snap.session.status,
        SessionStatus::Succeeded,
        "L1 must reach Succeeded, got {:?}",
        snap.session.status
    );
    assert!(
        snap.events.iter().any(|e| matches!(
            &e.kind,
            SessionEventKind::HumanInput { text } if text == ANSWER
        )),
        "HumanInput event must record the answer: {:?}",
        snap.events
    );
    // Ground truth: the pack *used* the answer (LifeOpsDemo summary embeds the title).
    let result = snap
        .session
        .result
        .as_ref()
        .expect("Succeeded session must carry GoalResult");
    assert!(
        result.summary.contains(ANSWER),
        "L1 ground truth: pack outcome must incorporate the human answer, not only narrate retry — summary={:?}",
        result.summary
    );
    assert!(
        result
            .artifacts
            .iter()
            .any(|a| a.to_lowercase().contains("l1-weekly-review-secret")
                || a.contains("L1-Weekly-Review-Secret")
                || a.contains("l1-weekly-review")),
        "artifact path must be derived from the answer title: {:?}",
        result.artifacts
    );
}

// ── L3 — parked reopen + POST message resumes; pack sees prior turns ────────

/// Pack that may resume from transcript and records `prior_turns()` on start (L3 ground truth).
struct ResumableSpyPack {
    saw: Arc<Mutex<Vec<(liberado_session::TurnAuthor, String)>>>,
}

#[async_trait]
impl DomainPackRunner for ResumableSpyPack {
    fn domain_id(&self) -> &str {
        "life"
    }

    async fn can_resume(&self, _ctx: &PackContext<'_>) -> bool {
        true
    }

    async fn run(
        &self,
        _id: &str,
        goal: &GoalSpec,
        ctx: &PackContext<'_>,
        _events: tokio::sync::mpsc::Sender<SessionEvent>,
        _inputs: liberado_session::InputChannel,
        _cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        let prior = ctx.prior_turns().await;
        *self.saw.lock().unwrap() = prior.clone();
        // On resume the human answer is already in the transcript (hub records it before run).
        // `run_session` also re-appends `goal.description` as a User turn at the start of every
        // run — so "last User turn" is often the description, not the answer. Prefer the last
        // User turn that is *not* the goal opener (the resume answer).
        let answer = prior
            .iter()
            .filter(|(a, t)| {
                matches!(a, liberado_session::TurnAuthor::User) && t.as_str() != goal.description
            })
            .map(|(_, t)| t.clone())
            .next_back()
            .unwrap_or_default();
        Ok(GoalResult {
            terminal: TerminalKind::Succeeded,
            summary: format!("resumed with answer '{answer}'"),
            artifacts: vec![],
            diagnostics: serde_json::json!({ "prior_turns": prior.len() }),
        })
    }
}

/// L3: session parked on disk with an open question (L2 shape) is answered via production
/// `POST .../message` (which falls through `send_input` Parked → `resume`). Pack sees prior turns.
#[tokio::test]
async fn l3_answer_parked_session_resumes_and_pack_sees_prior_turns() {
    let root = std::env::temp_dir().join(format!("liberado-t1-l3-{}", Ulid::new()));
    let session_id;
    {
        // First "daemon": interactive life pack reaches AwaitingInput, then dies.
        let harness = T1Harness::reopen_at(
            root.clone(),
            Arc::new(LifeOpsDemoRunner),
            life_config_with_ask_human(),
        )
        .await;
        let (status, body) = harness
            .post_json(
                "/api/goals",
                r#"{"description":"capture a note for L3","domain":"life","payload":{"interactive":true},"success_criteria":[]}"#,
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        session_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string();
        wait_status(&harness.goals, &session_id, |s| s.session.awaiting_input).await;
        drop(harness);
    }

    // Durable reopen: production store rehydrates as Parked.
    {
        let probe = SessionStore::open(&root).await;
        let id: Ulid = session_id.parse().unwrap();
        let header = probe.session(id).await.expect("rehydrate");
        assert_eq!(header.status, SessionStatus::Parked);
        assert!(header.awaiting_input);
    }

    let saw = Arc::new(Mutex::new(Vec::new()));
    let harness = T1Harness::reopen_at(
        root.clone(),
        Arc::new(ResumableSpyPack { saw: saw.clone() }),
        life_config_with_ask_human(),
    )
    .await;

    const ANSWER: &str = "L3-Parked-Resume-Title";
    let (msg_status, msg_body) = harness
        .post_json(
            &format!("/api/goals/{session_id}/message"),
            &format!(r#"{{"text":"{ANSWER}"}}"#),
        )
        .await;
    assert_eq!(
        msg_status,
        StatusCode::ACCEPTED,
        "L3 resume via goals_message must accept: {msg_body}"
    );

    let snap = wait_status(&harness.goals, &session_id, |s| {
        s.session.status.is_terminal()
    })
    .await;
    assert_eq!(
        snap.session.status,
        SessionStatus::Succeeded,
        "L3 must leave Parked and succeed after resume"
    );
    assert!(
        !snap.session.awaiting_input,
        "awaiting_input must clear after resume answer"
    );

    let seen = saw.lock().unwrap().clone();
    let texts: Vec<&str> = seen.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        texts
            .iter()
            .any(|t| t.contains("title") || t.contains("What should I")),
        "L3 ground truth: pack must see the prior assistant question on resume: {texts:?}"
    );
    assert!(
        texts.contains(&ANSWER),
        "L3 ground truth: pack must see the resume answer in prior_turns (recorded before run): {texts:?}"
    );
    let result = snap.session.result.as_ref().expect("result");
    assert!(
        result.summary.contains(ANSWER),
        "resumed pack summary must use the answer: {}",
        result.summary
    );
}

// ── L4 — irreversible pack refuses resume; session stays Parked ─────────────

/// Marker tool name: once this appears in prior events, `can_resume` is false (build started).
const L4_BUILD_TOOL: &str = "build";

/// Pack that refuses resume once a build-class tool event is on the transcript (irreversibility).
struct IrreversibleAfterBuildPack;

#[async_trait]
impl DomainPackRunner for IrreversibleAfterBuildPack {
    fn domain_id(&self) -> &str {
        "life"
    }

    async fn can_resume(&self, ctx: &PackContext<'_>) -> bool {
        let events = ctx.prior_events().await;
        !events.iter().any(|e| {
            matches!(
                &e.kind,
                SessionEventKind::ToolStarted { name, .. } if name == L4_BUILD_TOOL
            )
        })
    }

    async fn run(
        &self,
        _id: &str,
        _goal: &GoalSpec,
        _ctx: &PackContext<'_>,
        _events: tokio::sync::mpsc::Sender<SessionEvent>,
        _inputs: liberado_session::InputChannel,
        _cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        // Must never be re-entered when build has started — if we reach here on L4, the gate failed.
        Ok(GoalResult {
            terminal: TerminalKind::Succeeded,
            summary: "should-not-resume-after-build".into(),
            artifacts: vec![],
            diagnostics: serde_json::Value::Null,
        })
    }
}

/// L4: parked session whose pack has "started building" refuses resume; stays Parked; HTTP 409.
#[tokio::test]
async fn l4_build_started_refuses_resume_stays_parked() {
    use liberado_session::SessionRecordStore;

    let root = std::env::temp_dir().join(format!("liberado-t1-l4-{}", Ulid::new()));
    let session_id;
    {
        let harness = T1Harness::reopen_at(
            root.clone(),
            Arc::new(LifeOpsDemoRunner),
            life_config_with_ask_human(),
        )
        .await;
        let (status, body) = harness
            .post_json(
                "/api/goals",
                r#"{"description":"coding-shaped L4","domain":"life","payload":{"interactive":true},"success_criteria":[]}"#,
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        session_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string();
        wait_status(&harness.goals, &session_id, |s| s.session.awaiting_input).await;
        drop(harness);
    }

    // Plant the irreversibility marker *after* park (build had started before the ask landed).
    {
        let store = SessionStore::open(&root).await;
        store
            .push_event(SessionEvent::new(
                &session_id,
                SessionEventKind::ToolStarted {
                    name: L4_BUILD_TOOL.into(),
                    args_preview: "cargo test".into(),
                },
            ))
            .await;
        let id: Ulid = session_id.parse().unwrap();
        let header = store.session(id).await.expect("still on disk");
        assert_eq!(header.status, SessionStatus::Parked);
    }

    let harness = T1Harness::reopen_at(
        root.clone(),
        Arc::new(IrreversibleAfterBuildPack),
        life_config_with_ask_human(),
    )
    .await;

    let (msg_status, msg_body) = harness
        .post_json(
            &format!("/api/goals/{session_id}/message"),
            r#"{"text":"please resume anyway"}"#,
        )
        .await;
    assert_eq!(
        msg_status,
        StatusCode::CONFLICT,
        "L4 must refuse resume with 409, not re-run silently: {msg_body}"
    );
    assert!(
        msg_body.to_lowercase().contains("park") || msg_body.contains("Parked"),
        "error must say parked/refused, not finished: {msg_body}"
    );

    // Ground truth: still Parked on hub/store — not Succeeded from a silent re-run.
    let snap = harness
        .goals
        .snapshot(&session_id)
        .await
        .expect("session still listed");
    assert_eq!(
        snap.session.status,
        SessionStatus::Parked,
        "L4: session must stay Parked after refused resume"
    );
    assert!(
        snap.session.awaiting_input,
        "open question remains after refused resume"
    );
    assert!(
        snap.session
            .result
            .as_ref()
            .map(|r| !r.summary.contains("should-not-resume-after-build"))
            .unwrap_or(true),
        "pack must not have been re-run after irreversibility refusal"
    );
}

// ── L5 — grant without AskHuman → POST message is 403 ───────────────────────

/// L5: a session whose grant omits `AskHuman` never awaits input; `POST .../message` is **403**,
/// not 409 (timing) or 202 (accepted).
///
/// Exercises the production HTTP chain: `goals_start` resolves the grant from config, then
/// `goals_message` enforces `SendInputError::NotPermitted` → 403.
#[tokio::test]
async fn l5_message_without_ask_human_is_403_not_409() {
    let harness =
        T1Harness::with_pack(Arc::new(LifeOpsDemoRunner), life_config_without_ask_human()).await;

    // Start via the same HTTP path surfaces use. Config grants Write but not AskHuman.
    let (status, body) = harness
        .post_json(
            "/api/goals",
            r#"{"description":"unattended goal","domain":"life","payload":{"interactive":true},"success_criteria":["done"]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "start body: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = v["session_id"].as_str().expect("session_id").to_string();

    // Authority refusal, not timing: 403. A broken handler that mapped NotPermitted → 409 would fail.
    let (msg_status, msg_body) = harness
        .post_json(
            &format!("/api/goals/{id}/message"),
            r#"{"text":"let me help"}"#,
        )
        .await;
    assert_eq!(
        msg_status,
        StatusCode::FORBIDDEN,
        "L5 requires 403 for missing AskHuman; got {msg_status}, body={msg_body}"
    );
    assert_ne!(
        msg_status,
        StatusCode::CONFLICT,
        "409 is the timing answer, not authority"
    );
    assert_ne!(msg_status, StatusCode::ACCEPTED);

    // Ground truth: never parked waiting on a human who cannot answer.
    let snap = wait_status(&harness.goals, &id, |s| s.session.status.is_terminal()).await;
    assert!(
        !snap.session.awaiting_input,
        "session without AskHuman must never await input"
    );
}

// ── L8 — cancel → Cancelled on hub ground truth ─────────────────────────────

/// Pack that only terminates when cancel actually reaches it (mutation-testing lesson).
struct NeverEndingPack;

#[async_trait::async_trait]
impl DomainPackRunner for NeverEndingPack {
    fn domain_id(&self) -> &str {
        "life"
    }

    async fn run(
        &self,
        _id: &str,
        _goal: &GoalSpec,
        _ctx: &PackContext<'_>,
        _events: tokio::sync::mpsc::Sender<SessionEvent>,
        _inputs: liberado_session::InputChannel,
        mut cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        loop {
            if *cancel.borrow() {
                return Err(PackError::Cancelled);
            }
            if cancel.changed().await.is_err() {
                return Err(PackError::Cancelled);
            }
        }
    }
}

/// L8: `POST /api/goals/{id}/cancel` must stop the pack — hub snapshot reaches `Cancelled`.
/// A no-op cancel that only returns HTTP 2xx would hang or leave `Running`.
#[tokio::test]
async fn l8_cancel_reaches_cancelled_on_hub_ground_truth() {
    let harness =
        T1Harness::with_pack(Arc::new(NeverEndingPack), life_config_with_ask_human()).await;

    let (status, body) = harness
        .post_json(
            "/api/goals",
            r#"{"description":"work forever until cancelled","domain":"life","success_criteria":[]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_string();

    wait_status(&harness.goals, &id, |s| {
        s.session.status == SessionStatus::Running
    })
    .await;

    let (cancel_status, cancel_body) = harness
        .post_json(&format!("/api/goals/{id}/cancel"), "{}")
        .await;
    assert_eq!(
        cancel_status,
        StatusCode::ACCEPTED,
        "cancel HTTP path: {cancel_body}"
    );

    let snap = wait_status(&harness.goals, &id, |s| s.session.status.is_terminal()).await;
    assert_eq!(
        snap.session.status,
        SessionStatus::Cancelled,
        "L8: cancel must leave Cancelled on the hub, not only HTTP 202 — status={:?}",
        snap.session.status
    );
    // Store lens agrees (durable SessionStore behind the hub).
    let record = harness
        .goals
        .store()
        .get(&id)
        .await
        .expect("session must still be in the store");
    assert_eq!(record.status, SessionStatus::Cancelled);
}

// ── L10 — fork at turn N is a snapshot prefix ───────────────────────────────

/// L10: fork after turn *n* keeps the prefix; continuing the original does not move the fork.
#[tokio::test]
async fn l10_fork_holds_prefix_while_original_continues() {
    let harness = T1Harness::with_life_pack().await;
    let sessions = &harness.sessions;

    let conv = sessions
        .create_session(NewSession {
            title: Some("t1-fork-original".into()),
            ..Default::default()
        })
        .await
        .id;

    // Three user turns: q1/a1, q2/a2, q3/a3
    let mut parent = None;
    for (q, a) in [("q1", "a1"), ("q2", "a2"), ("q3", "a3")] {
        let u = sessions
            .append(
                conv,
                NewNode {
                    parent_id: parent,
                    author: Author::User,
                    message: Message::user(q),
                },
            )
            .await
            .unwrap();
        let a = sessions
            .append(
                conv,
                NewNode {
                    parent_id: Some(u.id),
                    author: Author::Assistant,
                    message: Message::assistant(a),
                },
            )
            .await
            .unwrap();
        parent = Some(a.id);
    }

    let (status, body) = harness
        .post_json(&format!("/api/sessions/{conv}/fork"), r#"{"after_turn":1}"#)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let fork: chat_client_contract::ForkResponse = serde_json::from_str(&body).unwrap();
    assert_eq!(fork.kept_turns, 1);
    assert_eq!(fork.total_turns, 3);
    let fork_id: Ulid = fork.id.parse().unwrap();

    let fork_path = sessions.leaf_path(fork_id, None).await.unwrap();
    assert_eq!(
        fork_path
            .iter()
            .map(|n| n.message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["q1", "a1"],
        "fork must hold only the prefix through turn 1"
    );

    // Continue the original — copy semantics: the fork must not grow.
    let original_path = sessions.leaf_path(conv, None).await.unwrap();
    let leaf = original_path.last().unwrap().id;
    sessions
        .append(
            conv,
            NewNode {
                parent_id: Some(leaf),
                author: Author::User,
                message: Message::user("q4-after-fork"),
            },
        )
        .await
        .unwrap();

    let fork_after = sessions.leaf_path(fork_id, None).await.unwrap();
    assert_eq!(
        fork_after
            .iter()
            .map(|n| n.message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["q1", "a1"],
        "continuing the original must not move the fork"
    );
    let original_after = sessions.leaf_path(conv, None).await.unwrap();
    assert!(
        original_after
            .iter()
            .any(|n| n.message.content == "q4-after-fork"),
        "original must have the new turn"
    );
    assert!(
        !fork_after
            .iter()
            .any(|n| n.message.content == "q4-after-fork"),
        "fork must not contain post-fork original turns"
    );
}

// ── L2 — durable reopen mid-awaiting_input → Parked + question visible ──────

/// L2: a session parked mid-question on a durable store survives "daemon restart" (drop hub,
/// reopen store from disk) as **Parked** with `awaiting_input` and the open question still visible.
///
/// Production boots `GoalSessionHub` over `SessionStore` JSONL; rehydrate coerces
/// non-terminal + awaiting → `Parked` (E6). An in-memory hub double alone would not prove this.
#[tokio::test]
async fn l2_reopen_store_while_awaiting_input_is_parked_with_question() {
    let root = std::env::temp_dir().join(format!("liberado-t1-l2-{}", Ulid::new()));
    let session_id;
    {
        let sessions = Arc::new(SessionStore::open(&root).await);
        let mut hub = GoalSessionHub::new(SessionStore::clone(&sessions));
        hub.register_pack(Arc::new(LifeOpsDemoRunner));
        let goals = Arc::new(hub);

        let grant = SessionGrant {
            capabilities: life_capabilities_with_ask_human(),
            profile: None,
            overrides: serde_json::Value::Null,
        };
        session_id = goals
            .start_with_grant(
                GoalSpec {
                    id: None,
                    description: "capture a note for L2 reopen".into(),
                    success_criteria: vec![],
                    domain: DomainHint::Life,
                    max_turns: 0,
                    max_idle_secs: None,
                    origin: None,
                    profile: None,
                    payload: serde_json::json!({ "interactive": true }),
                },
                grant,
            )
            .await
            .expect("start interactive life session");

        wait_status(&goals, &session_id, |s| s.session.awaiting_input).await;
        let live = goals.snapshot(&session_id).await.expect("live snapshot");
        assert!(live.session.awaiting_input);
        assert!(
            live.events.iter().any(|e| matches!(
                &e.kind,
                SessionEventKind::AwaitingInput { prompt, .. }
                    if prompt.contains("title")
            )),
            "live session must hold the AwaitingInput prompt before we drop the hub"
        );
        // Drop hub + first store handle — models a daemon restart (in-memory engine gone).
        drop(goals);
        drop(sessions);
    }

    // Reopen production store type from the same directory (rehydrate path).
    let reopened = SessionStore::open(&root).await;
    let id: Ulid = session_id.parse().expect("session id is a ULID");
    let header = reopened
        .session(id)
        .await
        .expect("session must rehydrate from disk");
    assert_eq!(
        header.status,
        SessionStatus::Parked,
        "L2: awaiting session must reopen as Parked, not Failed/Running — got {:?}",
        header.status
    );
    assert!(
        header.awaiting_input,
        "L2: open question must survive reopen (awaiting_input)"
    );

    // Ground truth for "question still visible": event stream + transcript, not narration alone.
    use liberado_session::SessionRecordStore;
    let events = reopened
        .events(&session_id)
        .await
        .expect("events must rehydrate");
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            SessionEventKind::AwaitingInput { prompt, .. } if prompt.contains("title")
        )),
        "AwaitingInput event must be on the durable log after reopen: {events:?}"
    );
    let turns = reopened.turns(&session_id).await;
    assert!(
        turns
            .iter()
            .any(|(_, text)| text.contains("What should I title") || text.contains("title")),
        "assistant question turn must be in the transcript: {turns:?}"
    );
}

// ── L6 — Write grant both-arms via RiskGatedToolRuntime ─────────────────────

/// Spy inner runtime: ground truth for whether the write tool actually ran.
struct SpyWriteRuntime {
    ran: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl ToolRuntime for SpyWriteRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        vec![]
    }
    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        self.ran.lock().unwrap().push(call.name.clone());
        Ok("spy: wrote".into())
    }
}

fn vault_write_descriptor() -> McpDescriptor {
    McpDescriptor {
        name: "vault".into(),
        description: "path-addressed vault MCP".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: Some("path".into()),
        write_tools: vec!["write_note".into()],
    }
}

fn write_note_call() -> ToolInvocation {
    ToolInvocation::new(
        "t1-l6",
        "vault:write_note",
        serde_json::json!({"path": "tasks/a.md", "content": "x"}),
    )
}

fn risk_gate(caps: CapabilitySet) -> (RiskGatedToolRuntime, Arc<std::sync::Mutex<Vec<String>>>) {
    let ran = Arc::new(std::sync::Mutex::new(Vec::new()));
    let inner = Arc::new(SpyWriteRuntime { ran: ran.clone() });
    let rt = RiskGatedToolRuntime::new(
        inner,
        caps,
        vec![("vault".into(), Consequence::Reversible)],
        vec![vault_write_descriptor()],
        // Agent-writable so *risk* guards pass; refusal must be authority (Write grant), not risk.
        vec![("tasks".to_string(), WriteClass::AgentWritable)],
        std::env::temp_dir(),
        "write a note".into(),
        "t1-l6".into(),
        ProposalSigner::random(),
        "default",
    );
    (rt, ran)
}

/// L6 refuse arm: `Read` + `ExecuteMcp` without `Write` → invoke is `Err`, spy never runs.
#[tokio::test]
async fn l6_without_write_refuses_and_spy_tool_never_runs() {
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("vault".into()),
        Capability::Read(Zone::vault("tasks")),
    ]);
    let (rt, ran) = risk_gate(caps);

    let err = rt
        .invoke(&write_note_call())
        .await
        .expect_err("L6 refuse: Write omitted must refuse the write");
    assert!(
        err.contains("not authorized"),
        "authority refusal, not a soft downgrade: {err}"
    );
    assert!(err.contains("tasks"), "refusal should name the zone: {err}");
    assert!(
        ran.lock().unwrap().is_empty(),
        "inner mock write tool must never run when Write is withheld"
    );
}

/// L6 allow arm (positive control): with `Write(tasks)` the spy **does** run — refuse arm is not
/// a blanket always-deny gate.
#[tokio::test]
async fn l6_with_write_allows_and_spy_tool_runs() {
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("vault".into()),
        Capability::Read(Zone::vault("tasks")),
        Capability::Write(Zone::vault("tasks")),
    ]);
    let (rt, ran) = risk_gate(caps);

    let out = rt
        .invoke(&write_note_call())
        .await
        .expect("L6 allow: granted Write must pass the gate");
    assert_eq!(out, "spy: wrote");
    assert_eq!(
        *ran.lock().unwrap(),
        vec!["vault:write_note".to_string()],
        "positive control: the write tool must actually have run"
    );
}
