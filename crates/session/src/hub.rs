//! Goal session hub: register packs, start/cancel sessions, fan out events.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use crate::event::{SessionEvent, SessionEventKind};
use crate::goal::{GoalResult, GoalSessionRecord, GoalSpec, SessionStatus, TerminalKind};
use crate::runner::{DomainPackRunner, PackError};
use crate::store::GoalSessionStore;

/// Snapshot returned to HTTP clients.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSnapshot {
    pub session: GoalSessionRecord,
    pub events: Vec<SessionEvent>,
}

/// In-process goal session orchestrator (not the coding loop itself).
pub struct GoalSessionHub {
    store: GoalSessionStore,
    packs: HashMap<String, Arc<dyn DomainPackRunner>>,
    cancels: tokio::sync::Mutex<HashMap<String, watch::Sender<bool>>>,
}

impl GoalSessionHub {
    pub fn new(store: GoalSessionStore) -> Self {
        Self {
            store,
            packs: HashMap::new(),
            cancels: tokio::sync::Mutex::new(HashMap::new()),
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

    /// Start a goal session asynchronously. Returns the session id immediately.
    pub async fn start(self: &Arc<Self>, mut goal: GoalSpec) -> Result<String, String> {
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

        let record = GoalSessionRecord::new(goal.clone());
        let id = record.id.clone();
        goal.id = Some(id.clone());
        self.store.insert(record).await;

        let (cancel_tx, cancel_rx) = watch::channel(false);
        self.cancels.lock().await.insert(id.clone(), cancel_tx);

        let hub = Arc::clone(self);
        let session_id = id.clone();
        let pack = self
            .packs
            .get(&domain)
            .expect("checked above")
            .clone();

        tokio::spawn(async move {
            hub.run_session(session_id, goal, pack, cancel_rx).await;
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

        let result = pack
            .run(&session_id, &goal, tx.clone(), cancel)
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

        self.store
            .finish(&session_id, status, goal_result)
            .await;
        self.cancels.lock().await.remove(&session_id);
    }
}


