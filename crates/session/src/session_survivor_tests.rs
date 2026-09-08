//! Mutation-focused session-kernel tests.
//!
//! These stay beside the production modules so the package mutation campaign includes them.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, Notify, broadcast, mpsc, watch};

use crate::runner::{DomainPackRunner, InputChannel, PackContext, PackError};
use crate::{
    DomainHint, GoalResult, GoalSessionHub, GoalSessionRecord, GoalSessionStore, GoalSpec,
    ReviewerKind, SessionAlert, SessionEvent, SessionEventKind, SessionGrant, SessionRecordStore,
    SessionStatus, TerminalKind, TurnAuthor, Visibility, check_session_invariants,
};

fn goal(id: &str) -> GoalSpec {
    GoalSpec {
        id: Some(id.into()),
        description: "exercise session lifecycle".into(),
        success_criteria: vec![],
        domain: DomainHint::Life,
        max_turns: 0,
        max_idle_secs: None,
        origin: None,
        profile: None,
        payload: serde_json::Value::Null,
    }
}

async fn wait_for_status(hub: &GoalSessionHub, id: &str, wanted: SessionStatus) {
    for _ in 0..100 {
        if hub
            .snapshot(id)
            .await
            .map(|snapshot| snapshot.session.status)
            == Some(wanted)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{id} did not reach {wanted:?}");
}

async fn wait_for_hosts(hub: &GoalSessionHub, wanted: usize) {
    for _ in 0..100 {
        if hub.in_flight_count().await == wanted {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("hub did not reach {wanted} live hosts");
}

struct ImmediatePack;

#[async_trait::async_trait]
impl DomainPackRunner for ImmediatePack {
    fn domain_id(&self) -> &str {
        "life"
    }

    async fn run(
        &self,
        _id: &str,
        _goal: &GoalSpec,
        _ctx: &PackContext<'_>,
        _events: mpsc::Sender<SessionEvent>,
        _inputs: InputChannel,
        _cancel: watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        Ok(GoalResult {
            terminal: TerminalKind::Succeeded,
            summary: "done".into(),
            artifacts: vec![],
            diagnostics: serde_json::Value::Null,
        })
    }
}

struct CooperativePack;

#[async_trait::async_trait]
impl DomainPackRunner for CooperativePack {
    fn domain_id(&self) -> &str {
        "life"
    }

    async fn run(
        &self,
        _id: &str,
        _goal: &GoalSpec,
        _ctx: &PackContext<'_>,
        _events: mpsc::Sender<SessionEvent>,
        _inputs: InputChannel,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        loop {
            if *cancel.borrow() || cancel.changed().await.is_err() {
                return Err(PackError::Cancelled);
            }
        }
    }
}

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
        _events: mpsc::Sender<SessionEvent>,
        _inputs: InputChannel,
        _cancel: watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        std::future::pending().await
    }
}

struct DefaultSubscriberStore;

#[async_trait::async_trait]
impl SessionRecordStore for DefaultSubscriberStore {
    async fn insert(&self, _record: GoalSessionRecord) {}

    async fn insert_if_absent(&self, _record: GoalSessionRecord) -> crate::InsertOutcome {
        crate::InsertOutcome::Inserted
    }

    async fn get(&self, _id: &str) -> Option<GoalSessionRecord> {
        None
    }

    async fn list(&self) -> Vec<GoalSessionRecord> {
        vec![]
    }

    async fn events(&self, _id: &str) -> Option<Vec<SessionEvent>> {
        None
    }

    async fn subscribe(
        &self,
        _id: &str,
    ) -> Option<(Vec<SessionEvent>, broadcast::Receiver<SessionEvent>)> {
        None
    }

    async fn push_event(&self, _event: SessionEvent) {}

    async fn append_turn(&self, _session_id: &str, _author: TurnAuthor, _content: String) {}

    async fn turns(&self, _session_id: &str) -> Vec<(TurnAuthor, String)> {
        vec![]
    }

    async fn set_status(&self, _id: &str, _status: SessionStatus) {}

    async fn finish(&self, _id: &str, _status: SessionStatus, _result: GoalResult) {}
}

#[test]
fn origin_constructors_and_invariants_reject_invalid_session_state() {
    let conversation = crate::SessionOrigin::from_conversation("conversation-42");
    assert_eq!(
        conversation.conversation_id.as_deref(),
        Some("conversation-42")
    );
    assert_eq!(conversation.correlation_id, None);

    let correlation = crate::SessionOrigin::from_correlation("dispatch-42");
    assert_eq!(correlation.conversation_id, None);
    assert_eq!(correlation.correlation_id.as_deref(), Some("dispatch-42"));

    let mut record = GoalSessionRecord::new(goal("invalid"));
    record.status = SessionStatus::Pending;
    record.finished_at = Some(chrono::Utc::now());
    assert!(check_session_invariants(&record).is_err());

    let mut background_waiting =
        GoalSessionRecord::background(goal("background"), SessionGrant::default());
    background_waiting.awaiting_input = true;
    assert!(check_session_invariants(&background_waiting).is_err());
}

#[test]
fn reviewer_kind_display_is_the_stable_wire_label() {
    assert_eq!(ReviewerKind::Gatekeeper.to_string(), "gatekeeper");
    assert_eq!(ReviewerKind::Fresh.to_string(), "fresh");
    assert_eq!(ReviewerKind::Strategist.to_string(), "strategist");
}

#[tokio::test]
async fn stores_without_an_event_bus_report_zero_subscribers() {
    assert_eq!(
        DefaultSubscriberStore.live_subscriber_count("unused").await,
        0
    );
}

#[tokio::test]
async fn pack_context_forwards_transcript_events_and_profile_configuration() {
    let store = GoalSessionStore::new();
    let session_id = "context-forwarding";
    store.insert(GoalSessionRecord::new(goal(session_id))).await;
    store
        .push_event(SessionEvent::new(
            session_id,
            SessionEventKind::Progress {
                message: "started".into(),
            },
        ))
        .await;

    let grant = SessionGrant {
        profile: Some("focused".into()),
        overrides: serde_json::json!({"temperature": 0.2}),
        ..Default::default()
    };
    let context = PackContext::new(&grant, Arc::new(store.clone()), session_id);
    context
        .record_turn(TurnAuthor::Assistant, "draft ready")
        .await;

    assert_eq!(
        context.prior_turns().await,
        vec![(TurnAuthor::Assistant, "draft ready".into())]
    );
    assert_eq!(context.prior_events().await.len(), 1);
    assert_eq!(
        context.overrides(),
        &serde_json::json!({"temperature": 0.2})
    );
    assert_eq!(context.profile(), Some("focused"));

    struct DefaultResume;
    #[async_trait::async_trait]
    impl DomainPackRunner for DefaultResume {
        fn domain_id(&self) -> &str {
            "default-resume"
        }

        async fn run(
            &self,
            _id: &str,
            _goal: &GoalSpec,
            _ctx: &PackContext<'_>,
            _events: mpsc::Sender<SessionEvent>,
            _inputs: InputChannel,
            _cancel: watch::Receiver<bool>,
        ) -> Result<GoalResult, PackError> {
            unreachable!("can_resume must not run the pack")
        }
    }

    assert!(!DefaultResume.can_resume(&context).await);
}

#[tokio::test]
async fn durable_replay_applies_human_input_and_trait_methods_change_observable_state() {
    let dir = std::env::temp_dir().join(format!("liberado-session-survivor-{}", ulid::Ulid::new()));
    let id = "durable-input";
    {
        let store = GoalSessionStore::open(&dir).await;
        store.insert(GoalSessionRecord::new(goal(id))).await;
        store
            .push_event(SessionEvent::new(
                id,
                SessionEventKind::AwaitingInput {
                    prompt: "continue?".into(),
                    options: vec![],
                },
            ))
            .await;
        store
            .push_event(SessionEvent::new(
                id,
                SessionEventKind::HumanInput { text: "yes".into() },
            ))
            .await;
    }

    let replayed = GoalSessionStore::open(&dir).await;
    assert!(!replayed.get(id).await.unwrap().awaiting_input);

    let (_history, _receiver) = replayed.subscribe(id).await.unwrap();
    assert_eq!(
        SessionRecordStore::live_subscriber_count(&replayed, id).await,
        1
    );
    SessionRecordStore::set_status(&replayed, id, SessionStatus::Cancelled).await;
    assert_eq!(
        replayed.get(id).await.unwrap().status,
        SessionStatus::Cancelled
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn background_start_returns_the_requested_id_and_marks_the_record_background() {
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(ImmediatePack));
    let hub = Arc::new(hub);

    let id = hub
        .start_background(goal("background-start"), SessionGrant::default())
        .await
        .unwrap();
    assert_eq!(id, "background-start");
    assert_eq!(
        hub.snapshot(&id).await.unwrap().session.visibility,
        Visibility::Background
    );
    assert_eq!(
        hub.await_terminal(&id).await.unwrap().session.status,
        SessionStatus::Succeeded
    );
}

#[tokio::test]
async fn await_terminal_ignores_progress_and_returns_only_after_the_finished_event() {
    struct GatedPack {
        progressed: Arc<Notify>,
        finish: Arc<Notify>,
        held_events: Arc<Mutex<Option<mpsc::Sender<SessionEvent>>>>,
    }

    #[async_trait::async_trait]
    impl DomainPackRunner for GatedPack {
        fn domain_id(&self) -> &str {
            "life"
        }

        async fn run(
            &self,
            id: &str,
            _goal: &GoalSpec,
            _ctx: &PackContext<'_>,
            events: mpsc::Sender<SessionEvent>,
            _inputs: InputChannel,
            _cancel: watch::Receiver<bool>,
        ) -> Result<GoalResult, PackError> {
            events
                .send(SessionEvent::new(
                    id,
                    SessionEventKind::Progress {
                        message: "still working".into(),
                    },
                ))
                .await
                .unwrap();
            self.progressed.notify_one();
            self.finish.notified().await;
            *self.held_events.lock().await = Some(events.clone());
            Ok(GoalResult {
                terminal: TerminalKind::Succeeded,
                summary: "completed after progress".into(),
                artifacts: vec![],
                diagnostics: serde_json::Value::Null,
            })
        }
    }

    let progressed = Arc::new(Notify::new());
    let finish = Arc::new(Notify::new());
    let held_events = Arc::new(Mutex::new(None));
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(GatedPack {
        progressed: progressed.clone(),
        finish: finish.clone(),
        held_events: held_events.clone(),
    }));
    let hub = Arc::new(hub);
    let id = hub.start(goal("wait-for-terminal")).await.unwrap();
    let waiter = tokio::spawn({
        let hub = hub.clone();
        let id = id.clone();
        async move { hub.await_terminal(&id).await }
    });

    progressed.notified().await;
    assert_eq!(
        hub.snapshot(&id).await.unwrap().session.status,
        SessionStatus::Running
    );
    finish.notify_one();
    let snapshot = tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("terminal event must wake await_terminal")
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.session.status, SessionStatus::Succeeded);
    held_events.lock().await.take();
}

#[tokio::test]
async fn parking_signals_the_pack_and_records_parked_not_cancelled() {
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(CooperativePack));
    let hub = Arc::new(hub);
    let id = hub.start(goal("park-one")).await.unwrap();
    wait_for_hosts(&hub, 1).await;

    hub.park(&id).await.unwrap();
    wait_for_status(&hub, &id, SessionStatus::Parked).await;
    assert_eq!(
        hub.snapshot(&id).await.unwrap().session.status,
        SessionStatus::Parked
    );
}

#[tokio::test]
async fn host_accounting_parks_every_live_session_during_shutdown() {
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(NeverEndingPack));
    let hub = Arc::new(hub);
    let first = hub.start(goal("host-one")).await.unwrap();
    let second = hub.start(goal("host-two")).await.unwrap();
    wait_for_hosts(&hub, 2).await;
    wait_for_status(&hub, &first, SessionStatus::Running).await;
    wait_for_status(&hub, &second, SessionStatus::Running).await;

    let mut ids = hub.in_flight_ids().await;
    ids.sort();
    assert_eq!(ids, vec![first.clone(), second.clone()]);
    assert_eq!(hub.park_all_in_flight().await, 2);
    assert_eq!(hub.force_park_still_hosted().await, 2);
    assert_eq!(
        hub.snapshot(&first).await.unwrap().session.status,
        SessionStatus::Parked
    );
    assert_eq!(
        hub.snapshot(&second).await.unwrap().session.status,
        SessionStatus::Parked
    );
}

#[derive(Default)]
struct AlertSpy {
    calls: Mutex<Vec<(String, String)>>,
}

#[async_trait::async_trait]
impl SessionAlert for AlertSpy {
    async fn session_needs_you(&self, session_id: &str, prompt: &str) {
        self.calls
            .lock()
            .await
            .push((session_id.into(), prompt.into()));
    }
}

#[tokio::test]
async fn unwatched_input_prompt_sends_one_out_of_band_alert() {
    struct PromptPack;

    #[async_trait::async_trait]
    impl DomainPackRunner for PromptPack {
        fn domain_id(&self) -> &str {
            "life"
        }

        async fn run(
            &self,
            id: &str,
            _goal: &GoalSpec,
            _ctx: &PackContext<'_>,
            events: mpsc::Sender<SessionEvent>,
            _inputs: InputChannel,
            _cancel: watch::Receiver<bool>,
        ) -> Result<GoalResult, PackError> {
            events
                .send(SessionEvent::new(
                    id,
                    SessionEventKind::AwaitingInput {
                        prompt: "choose a title".into(),
                        options: vec![],
                    },
                ))
                .await
                .unwrap();
            Ok(GoalResult {
                terminal: TerminalKind::Succeeded,
                summary: "prompt was delivered".into(),
                artifacts: vec![],
                diagnostics: serde_json::Value::Null,
            })
        }
    }

    let alert = Arc::new(AlertSpy::default());
    let mut hub = GoalSessionHub::new(GoalSessionStore::new()).with_alert(alert.clone());
    hub.register_pack(Arc::new(PromptPack));
    let hub = Arc::new(hub);
    let id = hub.start(goal("unwatched-alert")).await.unwrap();
    wait_for_status(&hub, &id, SessionStatus::Succeeded).await;

    assert_eq!(
        alert.calls.lock().await.as_slice(),
        &[(id, "choose a title".into())]
    );
}
