//! Goal session hub: register packs, start/cancel sessions, fan out events.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, watch};
use tracing::{debug, info, warn};

use crate::event::{SessionEvent, SessionEventKind};
use crate::goal::{
    GoalResult, GoalSessionRecord, GoalSpec, SessionGrant, SessionStatus, TerminalKind, Visibility,
};
use crate::record_store::{SessionRecordStore, TurnAuthor};
use crate::runner::{DomainPackRunner, HumanInput, InputChannel, PackContext, PackError};

/// Bound on how many un-consumed human inputs a session buffers before back-pressure. Interactive
/// sessions consume one per await point, so a small buffer is plenty.
const INPUT_CHANNEL_CAPACITY: usize = 16;

/// Snapshot returned to HTTP clients.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSnapshot {
    pub session: GoalSessionRecord,
    pub events: Vec<SessionEvent>,
}

/// Why a [`GoalSessionHub::send_input`] delivery could not happen. Typed (not a `String`) so the
/// HTTP layer can map it to a status code — 404 for [`Unknown`](Self::Unknown), 409 for
/// [`Terminal`](Self::Terminal), 403 for [`NotPermitted`](Self::NotPermitted) — rather than
/// string-matching a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendInputError {
    /// No goal session with this id exists (or ever did).
    Unknown,
    /// The session exists but has already reached a terminal state, so it accepts no more input.
    Terminal,
    /// The session's grant omits [`Capability::AskHuman`], so it may never receive human input —
    /// not a timing problem but an authority one (S6).
    NotPermitted,
    /// The daemon restarted while the session was waiting on a human, so it replayed as
    /// [`SessionStatus::Parked`] (E6). It is **not** finished — the question it holds for you is
    /// still there — but no pack is hosting it, so there is nothing to deliver an answer to until
    /// the pack can be resumed (E6-c).
    ///
    /// Distinct from [`Terminal`](Self::Terminal) on purpose: telling someone their parked session
    /// "has already finished" is a lie, and it is the difference between "this is dead, start over"
    /// and "this is waiting, and cannot be answered yet".
    Parked,
    /// The session's input channel closed underneath us — a rare teardown race between the lookup
    /// and the send.
    Closed,
}

impl std::fmt::Display for SendInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => write!(f, "no such goal session"),
            Self::Terminal => {
                write!(f, "goal session has already finished — not accepting input")
            }
            Self::NotPermitted => write!(
                f,
                "goal session's grant does not include AskHuman — it cannot receive human input"
            ),
            Self::Parked => write!(
                f,
                "goal session is parked: the daemon restarted while it was waiting on you, so the \
                 question it holds is still there but no pack is running to receive the answer. \
                 It has NOT finished — it cannot be resumed yet (E6-c)"
            ),
            Self::Closed => write!(f, "goal session input channel is closed"),
        }
    }
}

impl std::error::Error for SendInputError {}

/// Out-of-band ping when a session awaits input and nobody is watching the event bus (E5).
///
/// Implemented by the composition root (e.g. wrapping `liberado_notify::Notifier`) so the kernel
/// stays free of a concrete channel. Best-effort: failures are logged, never fatal.
#[async_trait::async_trait]
pub trait SessionAlert: Send + Sync {
    async fn session_needs_you(&self, session_id: &str, prompt: &str);
}

/// In-process goal session orchestrator (not the coding loop itself).
pub struct GoalSessionHub {
    /// The store seam (S5′) — a trait, not a concrete type, so the converged `Session` store can
    /// back the same kernel without the hub knowing which engine it is talking to.
    store: Arc<dyn SessionRecordStore>,
    packs: HashMap<String, Arc<dyn DomainPackRunner>>,
    cancels: tokio::sync::Mutex<HashMap<String, watch::Sender<bool>>>,
    /// Live sessions' inbound-input senders, keyed by id. Present only while a session runs;
    /// removed at teardown (like `cancels`), so `send_input` to a finished session fails cleanly.
    inputs: tokio::sync::Mutex<HashMap<String, mpsc::Sender<HumanInput>>>,
    /// Sessions whose cooperative stop was requested as a **park**, not a cancel.
    ///
    /// Park and cancel use the same stop signal — a pack has one way to be asked to wind down, and
    /// giving it two would mean every pack has to handle both correctly. What differs is the
    /// *disposition* the hub records when the pack returns, and this set is that intent. Cleared at
    /// teardown alongside `cancels`.
    park_requests: tokio::sync::Mutex<std::collections::HashSet<String>>,
    /// Optional out-of-band alert when a session awaits input with no live subscribers (E5).
    alert: Option<Arc<dyn SessionAlert>>,
}

impl GoalSessionHub {
    /// Build a hub over any [`SessionRecordStore`] — the in-memory/JSONL [`GoalSessionStore`], or
    /// (S5′) the converged `Session` store.
    pub fn new(store: impl SessionRecordStore + 'static) -> Self {
        Self {
            store: Arc::new(store),
            packs: HashMap::new(),
            cancels: tokio::sync::Mutex::new(HashMap::new()),
            inputs: tokio::sync::Mutex::new(HashMap::new()),
            park_requests: tokio::sync::Mutex::new(std::collections::HashSet::new()),
            alert: None,
        }
    }

    /// Attach an out-of-band alert for unwatched `AwaitingInput` events (E5).
    pub fn with_alert(mut self, alert: Arc<dyn SessionAlert>) -> Self {
        self.alert = Some(alert);
        self
    }

    pub fn store(&self) -> &Arc<dyn SessionRecordStore> {
        &self.store
    }

    pub fn register_pack(&mut self, runner: Arc<dyn DomainPackRunner>) {
        let id = runner.domain_id().to_string();
        info!(domain = %id, "registered goal session domain pack");
        self.packs.insert(id, runner);
    }

    pub fn registered_domains(&self) -> Vec<String> {
        self.packs.keys().cloned().collect()
    }

    /// Start a goal session with **zero authority** — the fail-safe default. Prefer
    /// [`start_with_grant`](Self::start_with_grant); this exists for callers (and tests) that mean
    /// "a session that may do nothing consequential and may not ask a human anything".
    pub async fn start(self: &Arc<Self>, goal: GoalSpec) -> Result<String, String> {
        self.start_with_grant(goal, SessionGrant::default()).await
    }

    /// Start a goal session under an explicit authority `grant` (S6). Returns the session id
    /// immediately. Visibility is [`Foreground`](Visibility::Foreground) — a human is watching.
    ///
    /// The grant is resolved by the *server* from the session's profile — the kernel never reads
    /// config. It is recorded on the session and never widened afterwards.
    ///
    /// **Interactivity is a capability**, not a session subtype
    /// (`docs/architecture/channels-and-interactivity.md`, Decision A): a grant without
    /// [`Capability::AskHuman`] gets no inbound input sender at all, so its pack receives an
    /// already-closed [`InputChannel`] and *cannot* block on a human who may not be there. This is
    /// the structural difference between an attended `/spawn` and an unattended cron.
    pub async fn start_with_grant(
        self: &Arc<Self>,
        goal: GoalSpec,
        grant: SessionGrant,
    ) -> Result<String, String> {
        self.start_inner(goal, grant, Visibility::Foreground).await
    }

    /// Start a **background** goal session — a cron, a webhook, a `delegate`d subagent. Same as
    /// [`start_with_grant`](Self::start_with_grant) but stamps [`Visibility::Background`].
    ///
    /// This is how unattended work becomes a *hosted* session (one-execution-engine plan E3/E4)
    /// rather than a read-only recording of work the hub never ran.
    pub async fn start_background(
        self: &Arc<Self>,
        goal: GoalSpec,
        grant: SessionGrant,
    ) -> Result<String, String> {
        self.start_inner(goal, grant, Visibility::Background).await
    }

    async fn start_inner(
        self: &Arc<Self>,
        mut goal: GoalSpec,
        grant: SessionGrant,
        visibility: Visibility,
    ) -> Result<String, String> {
        let domain = goal.domain.as_str().to_string();
        if !self.packs.contains_key(&domain) {
            return Err(format!(
                "no domain pack registered for '{domain}' (have: {:?})",
                self.registered_domains()
            ));
        }
        if goal.description.trim().is_empty() {
            return Err("goal description must not be empty".into());
        }

        let interactive = grant.grants_ask_human();
        let record = match visibility {
            Visibility::Foreground => GoalSessionRecord::with_grant(goal.clone(), grant),
            Visibility::Background => GoalSessionRecord::background(goal.clone(), grant),
        };
        let id = record.id.clone();
        goal.id = Some(id.clone());
        self.store.insert(record).await;

        let (cancel_tx, cancel_rx) = watch::channel(false);
        self.cancels.lock().await.insert(id.clone(), cancel_tx);

        let (input_tx, input_rx) = mpsc::channel::<HumanInput>(INPUT_CHANNEL_CAPACITY);
        // The AskHuman gate. Registering the sender is what makes a session reachable by
        // `send_input`; *dropping* it here (rather than storing it) closes the channel, so a pack
        // that tries to await input on an unpermitted session gets `Closed` immediately instead of
        // hanging until its idle budget expires.
        if interactive {
            self.inputs.lock().await.insert(id.clone(), input_tx);
        } else {
            drop(input_tx);
            debug!(
                session = %id,
                "session grant omits AskHuman — running non-interactively (input channel closed)"
            );
        }
        let idle_budget = goal.max_idle_secs.map(Duration::from_secs);
        let inputs = InputChannel::new(input_rx, idle_budget);

        let hub = Arc::clone(self);
        let session_id = id.clone();
        let pack = self.packs.get(&domain).expect("checked above").clone();

        tokio::spawn(async move {
            hub.run_session(session_id, goal, pack, inputs, cancel_rx)
                .await;
        });

        Ok(id)
    }

    /// Block until the session reaches a terminal status (or is unknown). Used by `delegate`, which
    /// is synchronous inside a chat turn and needs the pack's summary before it can reply.
    pub async fn await_terminal(&self, id: &str) -> Result<SessionSnapshot, String> {
        // Fast path: already done (or never started).
        if let Some(snap) = self.snapshot(id).await {
            if snap.session.status.is_terminal() {
                return Ok(snap);
            }
        } else {
            return Err(format!("no such goal session '{id}'"));
        }

        let (_history, mut rx) = self
            .store
            .subscribe(id)
            .await
            .ok_or_else(|| format!("no such goal session '{id}'"))?;

        loop {
            // Re-check after subscribe to close the race where finish landed between snapshot and
            // subscribe.
            if let Some(snap) = self.snapshot(id).await {
                if snap.session.status.is_terminal() {
                    return Ok(snap);
                }
            } else {
                return Err(format!("goal session '{id}' vanished"));
            }
            match rx.recv().await {
                Ok(ev) if matches!(ev.kind, SessionEventKind::SessionFinished { .. }) => {
                    return self
                        .snapshot(id)
                        .await
                        .ok_or_else(|| format!("goal session '{id}' vanished after finish"));
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    // Bus closed — session tore down; snapshot should be terminal.
                    return self
                        .snapshot(id)
                        .await
                        .ok_or_else(|| format!("goal session '{id}' ended without a snapshot"));
                }
            }
        }
    }

    pub async fn cancel(&self, id: &str) -> Result<(), String> {
        let map = self.cancels.lock().await;
        let tx = map
            .get(id)
            .ok_or_else(|| format!("session '{id}' not found or already finished"))?;
        let _ = tx.send(true);
        Ok(())
    }

    /// Ask a running session to **park**: wind down gracefully and land in
    /// [`SessionStatus::Parked`] rather than `Cancelled`.
    ///
    /// The difference from [`cancel`](Self::cancel) is entirely in what it *means*, not in how the
    /// pack is stopped. Both send the same cooperative stop signal, so the pack finishes its
    /// in-flight turn and exits at its own next checkpoint — packs need no new code path, and there
    /// is no second way to be interrupted that a pack could handle incorrectly. The hub then records
    /// `Parked` instead of `Cancelled`, and — unlike a terminal finish — **preserves
    /// `awaiting_input`**, so a session parked while holding a question still shows that question
    /// when you come back to it.
    ///
    /// Parking is a claim that the work is worth continuing. Whether it actually *can* continue is
    /// [`resume`](Self::resume)'s call, via [`DomainPackRunner::can_resume`]: the coding pack
    /// refuses once a build has started, because re-running it would redo real filesystem work with
    /// no checkpoint. That refusal happens at resume time rather than here because "can this be
    /// rebuilt from its transcript" depends on where the pack actually stopped, which is not
    /// knowable at the moment you ask it to.
    pub async fn park(&self, id: &str) -> Result<(), String> {
        // Clone the sender and release `cancels` before touching `park_requests`. Holding both at
        // once would establish a lock order that teardown (which takes them in the other order)
        // could deadlock against.
        let tx = {
            let map = self.cancels.lock().await;
            map.get(id)
                .cloned()
                .ok_or_else(|| format!("session '{id}' is not running (cannot park)"))?
        };
        // Record the intent BEFORE signalling. The pack may return almost immediately, and if the
        // stop landed first the hub would file a deliberate park as a cancel.
        self.park_requests.lock().await.insert(id.to_string());
        let _ = tx.send(true);
        Ok(())
    }

    /// Answer a **parked** session and put it back to work (E6-c).
    ///
    /// A session parked on a human across a daemon restart has no in-memory state left — no pack
    /// running, no input channel, no cancel handle. What it does have is its **transcript**, and for
    /// a pack that can rebuild itself from that, "resume" is simply: record the answer as a turn,
    /// then run the pack again. It reads its own dialogue back on start and picks up the
    /// conversation, rather than starting over and asking you everything a second time.
    ///
    /// The answer is recorded **before** the pack starts, which is the trick that makes this
    /// simple: by the time the pack reads `prior_turns()`, the human's latest answer is already part
    /// of the transcript, so there is no state to replay through the input channel and no window in
    /// which the pack could ask a question that has already been answered.
    ///
    /// Refuses when the pack says it cannot be resumed from a transcript
    /// ([`DomainPackRunner::can_resume`]) — the coding pack says no once the build has started,
    /// because re-running it would redo real filesystem work with no checkpoint. That refusal is
    /// honest rather than optimistic: the session stays parked and says so.
    pub async fn resume(
        self: &Arc<Self>,
        id: &str,
        answer: impl Into<String>,
    ) -> Result<(), SendInputError> {
        let answer = answer.into();
        let record = self.store.get(id).await.ok_or(SendInputError::Unknown)?;

        if !record.grant.grants_ask_human() {
            return Err(SendInputError::NotPermitted);
        }
        if record.status != SessionStatus::Parked {
            // Not parked: either it is live (and `send_input` is the right door), or it is over.
            return Err(if record.status.is_terminal() {
                SendInputError::Terminal
            } else {
                SendInputError::Closed
            });
        }

        let Some(pack) = self.packs.get(record.goal.domain.as_str()).cloned() else {
            return Err(SendInputError::Terminal);
        };
        let ctx = PackContext::new(&record.grant, self.store.clone(), id);
        if !pack.can_resume(&ctx).await {
            // The pack cannot rebuild itself from the transcript — for the coding pack this means
            // the build had already started. Leave it parked and honest rather than silently
            // re-running work that touched the filesystem.
            return Err(SendInputError::Parked);
        }

        // The answer joins the transcript first, so the pack sees it on `prior_turns()`.
        self.store
            .append_turn(id, TurnAuthor::User, answer.clone())
            .await;
        // …and the event, so a live surface renders it and `awaiting_input` clears.
        self.store
            .push_event(SessionEvent::new(
                id,
                SessionEventKind::HumanInput { text: answer },
            ))
            .await;

        let mut goal = record.goal.clone();
        goal.id = Some(id.to_string());

        let (cancel_tx, cancel_rx) = watch::channel(false);
        self.cancels.lock().await.insert(id.to_string(), cancel_tx);
        let (input_tx, input_rx) = mpsc::channel::<HumanInput>(INPUT_CHANNEL_CAPACITY);
        self.inputs.lock().await.insert(id.to_string(), input_tx);
        let idle_budget = goal.max_idle_secs.map(Duration::from_secs);
        let inputs = InputChannel::new(input_rx, idle_budget);

        let hub = Arc::clone(self);
        let session_id = id.to_string();
        tokio::spawn(async move {
            hub.run_session(session_id, goal, pack, inputs, cancel_rx)
                .await;
        });
        Ok(())
    }

    /// Deliver a human message into a running interactive session. Errors if the session is
    /// unknown or finished (its input sender was removed at teardown) — distinguished as
    /// [`SendInputError::Unknown`] vs [`SendInputError::Terminal`] so the HTTP layer can answer 404
    /// vs 409. The accepted input is **echoed into the transcript** as a
    /// [`SessionEventKind::HumanInput`] event here — so the history is complete regardless of what
    /// the pack does with it.
    pub async fn send_input(
        &self,
        id: &str,
        text: impl Into<String>,
    ) -> Result<(), SendInputError> {
        let text = text.into();
        // Clone the sender out before awaiting the send, so the lock isn't held across `.await`.
        let sender = {
            let map = self.inputs.lock().await;
            map.get(id).cloned()
        };
        let sender = match sender {
            Some(s) => s,
            // No live input sender — FOUR different reasons, and the caller needs to tell them
            // apart. Consult the record: a session that never held `AskHuman` was never *allowed*
            // input (403), which is a different fact from one that has since finished (409), one
            // that is parked mid-question across a restart (409, but *not* finished), or one that
            // never existed (404). Without these checks each masquerades as "you were too late",
            // when the truth may be "you were never allowed" or "it is still waiting for you".
            //
            // `Parked` was added by E6 and this match was not updated with it, so a parked session
            // was told it "has already finished" — the one thing it definitively has not done.
            None => {
                return Err(match self.store.get(id).await {
                    Some(record) if !record.grant.grants_ask_human() => {
                        SendInputError::NotPermitted
                    }
                    Some(record) if record.status == SessionStatus::Parked => {
                        SendInputError::Parked
                    }
                    Some(_) => SendInputError::Terminal,
                    None => SendInputError::Unknown,
                });
            }
        };
        sender
            .send(HumanInput::new(text.clone()))
            .await
            .map_err(|_| SendInputError::Closed)?;

        // Recorded **twice, on purpose**, because these are two different things:
        //
        // * the event is what a live subscriber sees (it is what clears the `awaiting_input` badge);
        // * the turn is what the human *said* — it belongs in the message DAG, so it is searchable
        //   and so a fork taken later carries it.
        //
        // The pack does not have to remember to do this: whatever a human says into a session is
        // dialogue by definition, so the kernel records it and no pack can forget to.
        self.store
            .append_turn(id, TurnAuthor::User, text.clone())
            .await;
        self.store
            .push_event(SessionEvent::new(id, SessionEventKind::HumanInput { text }))
            .await;
        Ok(())
    }

    pub async fn snapshot(&self, id: &str) -> Option<SessionSnapshot> {
        let session = self.store.get(id).await?;
        let events = self.store.events(id).await.unwrap_or_default();
        Some(SessionSnapshot { session, events })
    }

    pub async fn list(&self) -> Vec<GoalSessionRecord> {
        self.store.list().await
    }

    /// How many goal sessions currently have a live host (cancel handle present).
    ///
    /// Used by the server's graceful-shutdown drain so in-flight goals count toward the wait,
    /// not only chat turns. A session that has finished (or never started) is not counted.
    pub async fn in_flight_count(&self) -> usize {
        self.cancels.lock().await.len()
    }

    /// Ids of sessions currently hosted (same set as [`in_flight_count`](Self::in_flight_count)).
    pub async fn in_flight_ids(&self) -> Vec<String> {
        self.cancels.lock().await.keys().cloned().collect()
    }

    /// Ask every in-flight session to **park** (shutdown drain). Prefer this over cancel: parked
    /// sessions remain human-actionable after restart; cancelled ones do not.
    ///
    /// Returns how many park signals were accepted. Cooperative packs land in
    /// [`SessionStatus::Parked`]; the count is signals sent, not final status (status is observed
    /// after a short settle wait in the drain).
    pub async fn park_all_in_flight(&self) -> usize {
        let ids = self.in_flight_ids().await;
        let mut n = 0;
        for id in ids {
            if self.park(&id).await.is_ok() {
                n += 1;
            }
        }
        n
    }

    /// Durably record `Parked` for every session still hosted. The shutdown last word.
    ///
    /// [`park`](Self::park) only *signals*: `Parked` is filed by the session host after the pack
    /// returns, and the intent that distinguishes park from cancel lives in an in-memory set. At
    /// shutdown the process usually exits first — a session still running when the grace elapsed is
    /// typically blocked in a model call and cannot check its cancel channel until that returns —
    /// and then the store keeps `Running` for a session with **no host and no pending question**,
    /// which is precisely the state a human cannot act on.
    ///
    /// Writing the status here makes the on-disk record true whether or not the pack ever got to
    /// cooperate. Only sessions still recorded `Running` are touched, so one that finished during
    /// the settle window keeps its real terminal status; and if a forced-park session's pack does
    /// return before exit, `park_requests` still holds its id, so it files `Parked` again.
    ///
    /// Returns how many records were rewritten.
    pub async fn force_park_still_hosted(&self) -> usize {
        let ids = self.in_flight_ids().await;
        let mut n = 0;
        for id in ids {
            if self
                .store
                .get(&id)
                .await
                .is_some_and(|r| r.status == SessionStatus::Running)
            {
                self.store.set_status(&id, SessionStatus::Parked).await;
                self.store
                    .push_event(SessionEvent::new(
                        &id,
                        SessionEventKind::Progress {
                            message: "Session paused — the daemon restarted".into(),
                        },
                    ))
                    .await;
                n += 1;
            }
        }
        n
    }

    async fn run_session(
        &self,
        session_id: String,
        goal: GoalSpec,
        pack: Arc<dyn DomainPackRunner>,
        inputs: InputChannel,
        cancel: watch::Receiver<bool>,
    ) {
        self.store
            .set_status(&session_id, SessionStatus::Running)
            .await;

        let (tx, mut rx) = mpsc::channel::<SessionEvent>(64);
        let store = self.store.clone();
        let alert = self.alert.clone();
        let sid_for_pump = session_id.clone();
        let pump = tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                // E5: if the pack is waiting on a human and nobody has the stream open, ping them.
                // Checked *before* push so we don't count a subscriber that only appears via this
                // same event's fan-out (there isn't one — push creates no receivers).
                if let SessionEventKind::AwaitingInput { prompt, .. } = &ev.kind
                    && store.live_subscriber_count(&ev.session_id).await == 0
                    && let Some(alert) = &alert
                {
                    alert.session_needs_you(&ev.session_id, prompt).await;
                }
                store.push_event(ev).await;
            }
            // drain complete
            let _ = sid_for_pump;
        });

        let start = SessionEvent::new(
            &session_id,
            SessionEventKind::SessionStarted {
                domain: pack.domain_id().to_string(),
                description: goal.description.clone(),
            },
        );
        let _ = tx.send(start).await;

        // The grant recorded at `start_with_grant` is the session's authority for its whole life —
        // read back from the store rather than passed along, so there is exactly one source of
        // truth for what this session may do (and the non-widening invariant has something to hold).
        let grant = self
            .store
            .get(&session_id)
            .await
            .map(|r| r.grant)
            .unwrap_or_default();
        let ctx = PackContext::new(&grant, self.store.clone(), &session_id);

        // The goal opens the transcript, as the human's first turn — because it *is* one: it is the
        // thing the human said that started all this. Recording it here rather than in each pack
        // means every session's transcript begins with what it was actually for, and a fork taken at
        // any later point inherits it.
        self.store
            .append_turn(&session_id, TurnAuthor::User, goal.description.clone())
            .await;

        let result = pack
            .run(&session_id, &goal, &ctx, tx.clone(), inputs, cancel)
            .await;

        // Was this cooperative stop a park or a cancel? Same signal, different disposition — see
        // `park()`. Taken (and removed) here so a later run of the same id cannot inherit it.
        let parked = self.park_requests.lock().await.remove(&session_id);

        let (status, goal_result) = match result {
            // Was a hand-written four-arm match. A fifth `TerminalKind` would have compiled here
            // *and* silently gone missing, because nothing tied the two enums together.
            Ok(r) => (SessionStatus::from(r.terminal), r),
            Err(PackError::Cancelled) if parked => (
                SessionStatus::Parked,
                GoalResult {
                    terminal: TerminalKind::Cancelled, // unused: parked sessions never `finish()`
                    summary: "parked by client".into(),
                    artifacts: vec![],
                    diagnostics: serde_json::json!({}),
                },
            ),
            Err(PackError::Cancelled) => (
                SessionStatus::Cancelled,
                GoalResult {
                    terminal: TerminalKind::Cancelled,
                    summary: "cancelled by client".into(),
                    artifacts: vec![],
                    diagnostics: serde_json::json!({}),
                },
            ),
            Err(e) => {
                warn!(session_id = %session_id, error = %e, "goal session pack error");
                (
                    SessionStatus::Failed,
                    GoalResult {
                        terminal: TerminalKind::Failed,
                        summary: e.to_string(),
                        artifacts: vec![],
                        diagnostics: serde_json::json!({}),
                    },
                )
            }
        };

        // The outcome closes the transcript, as the session's last turn. A transcript that opens with
        // what was asked for and ends with what came of it reads as a conversation — which is what it
        // is — and a fork taken from the end inherits the answer rather than trailing off mid-thought.
        self.store
            .append_turn(
                &session_id,
                TurnAuthor::Assistant,
                goal_result.summary.clone(),
            )
            .await;

        let fin = SessionEvent::new(
            &session_id,
            SessionEventKind::SessionFinished {
                status: format!("{:?}", status).to_ascii_lowercase(),
                summary: goal_result.summary.clone(),
            },
        );
        let _ = tx.send(fin).await;
        drop(tx);
        let _ = pump.await;

        // A parked session has NOT finished, and `finish()` would say it had: it stamps
        // `finished_at` and clears `awaiting_input`, erasing the very question the human is coming
        // back to answer. `set_status` records the status and leaves both alone.
        if status == SessionStatus::Parked {
            self.store.set_status(&session_id, status).await;
        } else {
            self.store.finish(&session_id, status, goal_result).await;
        }
        self.cancels.lock().await.remove(&session_id);
        // Drop the input sender so any late `send_input` fails cleanly instead of blocking.
        self.inputs.lock().await.remove(&session_id);
    }
}
