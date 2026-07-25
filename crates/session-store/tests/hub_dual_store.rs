//! A4 — load-bearing `GoalSessionHub` behaviors on the **production** store type.
//!
//! `docs/architecture/failure-modes.md` §1: tests that only exercise
//! `GoalSessionStore` (in-memory) while production boots `SessionStore` create false confidence.
//!
//! This suite runs the **real hub** (`GoalSessionHub` API) against:
//! - `GoalSessionStore` — the double every pack unit test already uses
//! - `SessionStore` — durable JSONL, what the daemon boots
//!
//! Placement is intentional: `liberado-session` must not depend on `liberado-session-store`
//! (layer rules). The store crate is allowed to drive the hub over both implementations.
//!
//! Behaviors covered: **list**, **cancel**, **park → answer → resume** (prior turns), **rehydrate**.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use liberado_common::{Capability, CapabilitySet};
use liberado_session::{
    DomainHint, DomainPackRunner, GoalResult, GoalSessionHub, GoalSessionStore, GoalSpec,
    LifeOpsDemoRunner, PackContext, PackError, SessionEvent, SessionEventKind, SessionGrant,
    SessionStatus, TerminalKind, TurnAuthor, Visibility,
};
use liberado_session_store::SessionStore;
use tempfile::TempDir;

// ── Shared packs ─────────────────────────────────────────────────────────────

/// Never finishes unless cancel reaches the pack (mutation-testing lesson).
struct NeverEndingPack;

#[async_trait]
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

/// Resumable pack that records `prior_turns()` on start (resume ground truth).
struct ResumableSpyPack {
    saw: Arc<Mutex<Vec<(TurnAuthor, String)>>>,
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
        // Skip goal-description User turns re-appended at run_session start.
        let answer = prior
            .iter()
            .filter(|(a, t)| matches!(a, TurnAuthor::User) && t.as_str() != goal.description)
            .map(|(_, t)| t.clone())
            .next_back()
            .unwrap_or_default();
        Ok(GoalResult {
            terminal: TerminalKind::Succeeded,
            summary: format!("resumed:'{answer}'"),
            artifacts: vec![],
            diagnostics: serde_json::json!({ "prior_n": prior.len() }),
        })
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn life_goal(description: &str, interactive: bool) -> GoalSpec {
    GoalSpec {
        id: None,
        description: description.into(),
        success_criteria: vec![],
        domain: DomainHint::Life,
        max_turns: 0,
        max_idle_secs: None,
        origin: None,
        profile: None,
        payload: if interactive {
            serde_json::json!({ "interactive": true })
        } else {
            serde_json::Value::Null
        },
    }
}

fn attended_grant() -> SessionGrant {
    let mut capabilities = CapabilitySet::empty();
    capabilities.grant(Capability::AskHuman);
    capabilities.grant(Capability::Write(liberado_common::Zone::vault("tasks")));
    SessionGrant {
        capabilities,
        profile: None,
        overrides: serde_json::Value::Null,
    }
}

async fn wait_status(
    hub: &Arc<GoalSessionHub>,
    id: &str,
    pred: impl Fn(&liberado_session::SessionSnapshot) -> bool,
) -> liberado_session::SessionSnapshot {
    for _ in 0..400 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        if let Some(snap) = hub.snapshot(id).await
            && pred(&snap)
        {
            return snap;
        }
    }
    panic!("session {id} never matched wait predicate");
}

/// Run the same hub assertions once on in-memory and once on durable SessionStore.
async fn for_each_hub_store<F, Fut>(mut body: F)
where
    F: FnMut(&'static str, GoalSessionHub) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    // In-memory double (what pack unit tests use).
    body(
        "GoalSessionStore",
        GoalSessionHub::new(GoalSessionStore::new()),
    )
    .await;

    // Production durable store.
    let dir = TempDir::new().unwrap();
    let store = SessionStore::open(dir.path()).await;
    body("SessionStore", GoalSessionHub::new(store)).await;
    // Keep temp dir alive until body completes (dir dropped after).
    drop(dir);
}

// ── List ─────────────────────────────────────────────────────────────────────

/// A4 list: hub.list surfaces a session started through the hub, on **both** store types.
#[tokio::test]
async fn a4_list_surfaces_started_session_on_both_stores() {
    for_each_hub_store(|name, mut hub| async move {
        hub.register_pack(Arc::new(LifeOpsDemoRunner));
        let hub = Arc::new(hub);

        assert!(
            hub.list().await.is_empty(),
            "{name}: fresh hub list must be empty"
        );

        let id = hub
            .start(life_goal("a4 list goal", false))
            .await
            .unwrap_or_else(|e| panic!("{name}: start: {e}"));

        // Appear in list while running or after finish — either is joinable surface truth.
        let mut found = false;
        for _ in 0..200 {
            if hub.list().await.iter().any(|r| r.id == id) {
                found = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            found,
            "{name}: started session {id} must appear in hub.list()"
        );

        let _ = wait_status(&hub, &id, |s| s.session.status.is_terminal()).await;
        assert!(
            hub.list().await.iter().any(|r| r.id == id),
            "{name}: finished session must still list (surfaces rejoin history)"
        );
    })
    .await;
}

// ── Cancel ───────────────────────────────────────────────────────────────────

/// A4 cancel: hub.cancel stops a pack on both store types — Cancelled on hub ground truth.
#[tokio::test]
async fn a4_cancel_reaches_cancelled_on_both_stores() {
    for_each_hub_store(|name, mut hub| async move {
        hub.register_pack(Arc::new(NeverEndingPack));
        let hub = Arc::new(hub);

        let id = hub
            .start(life_goal("a4 cancel forever", false))
            .await
            .unwrap_or_else(|e| panic!("{name}: start: {e}"));

        wait_status(&hub, &id, |s| s.session.status == SessionStatus::Running).await;

        hub.cancel(&id)
            .await
            .unwrap_or_else(|e| panic!("{name}: cancel accepted: {e}"));

        let snap = wait_status(&hub, &id, |s| s.session.status.is_terminal()).await;
        assert_eq!(
            snap.session.status,
            SessionStatus::Cancelled,
            "{name}: cancel must reach Cancelled on the hub, not only Ok(())"
        );
        // Store lens agrees.
        let rec = hub
            .store()
            .get(&id)
            .await
            .unwrap_or_else(|| panic!("{name}: session still in store"));
        assert_eq!(rec.status, SessionStatus::Cancelled, "{name}");
    })
    .await;
}

// ── Park → answer → resume ───────────────────────────────────────────────────

/// A4 resume: parked session answered via hub.resume on both stores; pack sees prior turns.
#[tokio::test]
async fn a4_park_answer_resume_pack_sees_prior_turns_on_both_stores() {
    for_each_hub_store(|name, mut _hub| async move {
        // Build store + hub with a resumable pack; plant a parked session with transcript.
        // We construct the store explicitly so insert/append_turn use the same engine as the hub.
        // Keep TempDir in this async block so SessionStore's path lives for the whole body
        // (do not mem::forget — drop order is explicit when `_dir_guard` goes out of scope).
        let (_dir_guard, hub, saw, sid) = match name {
            "GoalSessionStore" => {
                let store = GoalSessionStore::new();
                let (hub, saw, sid) = plant_parked_and_hub(store, name).await;
                (None, hub, saw, sid)
            }
            "SessionStore" => {
                let dir = TempDir::new().unwrap();
                let store = SessionStore::open(dir.path()).await;
                let (hub, saw, sid) = plant_parked_and_hub(store, name).await;
                (Some(dir), hub, saw, sid)
            }
            _ => unreachable!(),
        };

        hub.resume(&sid, "A4-Resume-Answer")
            .await
            .unwrap_or_else(|e| panic!("{name}: resume: {e:?}"));

        let snap = wait_status(&hub, &sid, |s| s.session.status.is_terminal()).await;
        assert_eq!(
            snap.session.status,
            SessionStatus::Succeeded,
            "{name}: resume must leave Parked and succeed"
        );

        let seen = saw.lock().unwrap().clone();
        let texts: Vec<&str> = seen.iter().map(|(_, t)| t.as_str()).collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains("title") || t.contains("What")),
            "{name}: pack must see prior assistant question: {texts:?}"
        );
        assert!(
            texts.contains(&"A4-Resume-Answer"),
            "{name}: pack must see resume answer in prior_turns: {texts:?}"
        );
        assert!(
            snap.session
                .result
                .as_ref()
                .unwrap()
                .summary
                .contains("A4-Resume-Answer"),
            "{name}: result summary must use the answer"
        );
        // `_dir_guard` drops here after hub work completes.
        drop(_dir_guard);
    })
    .await;
}

async fn plant_parked_and_hub(
    store: impl liberado_session::SessionRecordStore + 'static,
    name: &str,
) -> (
    Arc<GoalSessionHub>,
    Arc<Mutex<Vec<(TurnAuthor, String)>>>,
    String,
) {
    use liberado_session::GoalSessionRecord;

    let sid = ulid::Ulid::new().to_string();
    let mut rec = GoalSessionRecord::new(life_goal("a4 park resume", false));
    rec.id = sid.clone();
    rec.grant = attended_grant();
    rec.status = SessionStatus::Parked;
    rec.awaiting_input = true;
    rec.visibility = Visibility::Foreground;

    store.insert(rec).await;
    store
        .append_turn(&sid, TurnAuthor::User, "a4 park resume".into())
        .await;
    store
        .append_turn(
            &sid,
            TurnAuthor::Assistant,
            "What should I title it?".into(),
        )
        .await;
    store
        .push_event(SessionEvent::new(
            &sid,
            SessionEventKind::AwaitingInput {
                prompt: "What should I title it?".into(),
                options: Vec::new(),
            },
        ))
        .await;

    let saw = Arc::new(Mutex::new(Vec::new()));
    let mut hub = GoalSessionHub::new(store);
    hub.register_pack(Arc::new(ResumableSpyPack { saw: saw.clone() }));
    let hub = Arc::new(hub);

    // Sanity: parked on the store the hub owns.
    let rec = hub.store().get(&sid).await.expect(name);
    assert_eq!(rec.status, SessionStatus::Parked, "{name}");
    assert!(rec.awaiting_input, "{name}");

    (hub, saw, sid)
}

// ── Rehydrate (SessionStore only — durable path) ─────────────────────────────

/// A4 rehydrate: awaiting session on durable SessionStore reopens as Parked with question visible.
/// In-memory store has no daemon-restart path; this is production-shaped only.
#[tokio::test]
async fn a4_rehydrate_awaiting_session_is_parked_with_question_on_session_store() {
    let dir = TempDir::new().unwrap();
    let session_id;
    {
        let store = SessionStore::open(dir.path()).await;
        let mut hub = GoalSessionHub::new(store);
        hub.register_pack(Arc::new(LifeOpsDemoRunner));
        let hub = Arc::new(hub);

        session_id = hub
            .start_with_grant(life_goal("a4 rehydrate note", true), attended_grant())
            .await
            .expect("start interactive");

        wait_status(&hub, &session_id, |s| s.session.awaiting_input).await;
        let live = hub.snapshot(&session_id).await.expect("live");
        assert!(
            live.events.iter().any(|e| matches!(
                &e.kind,
                SessionEventKind::AwaitingInput { prompt, .. } if prompt.contains("title")
            )),
            "live AwaitingInput before drop"
        );
        // Drop hub + first store handle — models daemon restart (in-memory engine gone).
        drop(hub);
    }

    let reopened = SessionStore::open(dir.path()).await;
    let id: ulid::Ulid = session_id.parse().unwrap();
    let header = reopened
        .session(id)
        .await
        .expect("session must rehydrate from disk");
    assert_eq!(
        header.status,
        SessionStatus::Parked,
        "A4: awaiting session must reopen as Parked — got {:?}",
        header.status
    );
    assert!(
        header.awaiting_input,
        "A4: open question must survive reopen"
    );

    use liberado_session::SessionRecordStore;
    let events = reopened
        .events(&session_id)
        .await
        .expect("events rehydrate");
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            SessionEventKind::AwaitingInput { prompt, .. } if prompt.contains("title")
        )),
        "AwaitingInput on durable log: {events:?}"
    );
    let turns = reopened.turns(&session_id).await;
    assert!(
        turns
            .iter()
            .any(|(_, t)| t.contains("title") || t.contains("What should I")),
        "question turn in transcript: {turns:?}"
    );
}
