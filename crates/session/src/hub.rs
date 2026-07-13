//! Goal session hub: register packs, start/cancel sessions, fan out events.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::event::{SessionEvent, SessionEventKind};
use crate::goal::{
    GoalResult, GoalSessionRecord, GoalSpec, SessionGrant, SessionStatus, TerminalKind,
};
use crate::runner::{DomainPackRunner, HumanInput, InputChannel, PackContext, PackError};
use crate::store::GoalSessionStore;

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
            Self::Closed => write!(f, "goal session input channel is closed"),
        }
    }
}

impl std::error::Error for SendInputError {}

/// In-process goal session orchestrator (not the coding loop itself).
pub struct GoalSessionHub {
    store: GoalSessionStore,
    packs: HashMap<String, Arc<dyn DomainPackRunner>>,
    cancels: tokio::sync::Mutex<HashMap<String, watch::Sender<bool>>>,
    /// Live sessions' inbound-input senders, keyed by id. Present only while a session runs;
    /// removed at teardown (like `cancels`), so `send_input` to a finished session fails cleanly.
    inputs: tokio::sync::Mutex<HashMap<String, mpsc::Sender<HumanInput>>>,
}

impl GoalSessionHub {
    pub fn new(store: GoalSessionStore) -> Self {
        Self {
            store,
            packs: HashMap::new(),
            cancels: tokio::sync::Mutex::new(HashMap::new()),
            inputs: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn store(&self) -> &GoalSessionStore {
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
    /// immediately.
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
        mut goal: GoalSpec,
        grant: SessionGrant,
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
        let record = GoalSessionRecord::with_grant(goal.clone(), grant);
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

    pub async fn cancel(&self, id: &str) -> Result<(), String> {
        let map = self.cancels.lock().await;
        let tx = map
            .get(id)
            .ok_or_else(|| format!("session '{id}' not found or already finished"))?;
        let _ = tx.send(true);
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
            // No live input sender — three different reasons, and the caller needs to tell them
            // apart. Consult the record: a session that never held `AskHuman` was never *allowed*
            // input (403), which is a different fact from one that has since finished (409) or one
            // that never existed (404). Without this check the first would masquerade as the
            // second, which reads as "you were too late" when the truth is "you were never allowed".
            None => {
                return Err(match self.store.get(id).await {
                    Some(record) if !record.grant.grants_ask_human() => {
                        SendInputError::NotPermitted
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
        let sid_for_pump = session_id.clone();
        let pump = tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
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
        let ctx = PackContext { grant: &grant };

        let result = pack
            .run(&session_id, &goal, &ctx, tx.clone(), inputs, cancel)
            .await;

        let (status, goal_result) = match result {
            Ok(r) => {
                let status = match r.terminal {
                    TerminalKind::Succeeded => SessionStatus::Succeeded,
                    TerminalKind::Failed => SessionStatus::Failed,
                    TerminalKind::Cancelled => SessionStatus::Cancelled,
                    TerminalKind::BudgetExhausted => SessionStatus::BudgetExhausted,
                };
                (status, r)
            }
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

        self.store.finish(&session_id, status, goal_result).await;
        self.cancels.lock().await.remove(&session_id);
        // Drop the input sender so any late `send_input` fails cleanly instead of blocking.
        self.inputs.lock().await.remove(&session_id);
    }
}
