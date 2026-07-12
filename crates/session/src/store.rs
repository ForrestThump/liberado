//! In-memory goal session store + per-session event buffers.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, broadcast};

use crate::event::SessionEvent;
use crate::goal::{GoalResult, GoalSessionRecord, SessionStatus};

const EVENT_CHANNEL_CAPACITY: usize = 256;

#[derive(Debug)]
struct SessionInner {
    record: GoalSessionRecord,
    events: Vec<SessionEvent>,
    bus: broadcast::Sender<SessionEvent>,
}

/// Process-local session store. Surfaces subscribe via [`subscribe`](Self::subscribe).
#[derive(Clone, Default)]
pub struct GoalSessionStore {
    inner: Arc<Mutex<HashMap<String, SessionInner>>>,
}

impl GoalSessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn insert(&self, record: GoalSessionRecord) {
        let id = record.id.clone();
        let (bus, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let mut map = self.inner.lock().await;
        map.insert(
            id,
            SessionInner {
                record,
                events: Vec::new(),
                bus,
            },
        );
    }

    pub async fn get(&self, id: &str) -> Option<GoalSessionRecord> {
        self.inner
            .lock()
            .await
            .get(id)
            .map(|s| s.record.clone())
    }

    pub async fn list(&self) -> Vec<GoalSessionRecord> {
        let mut rows: Vec<_> = self
            .inner
            .lock()
            .await
            .values()
            .map(|s| s.record.clone())
            .collect();
        rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        rows
    }

    pub async fn events(&self, id: &str) -> Option<Vec<SessionEvent>> {
        self.inner
            .lock()
            .await
            .get(id)
            .map(|s| s.events.clone())
    }

    /// Subscribe to live events. Also returns a snapshot of events so far (for catch-up).
    pub async fn subscribe(
        &self,
        id: &str,
    ) -> Option<(Vec<SessionEvent>, broadcast::Receiver<SessionEvent>)> {
        let map = self.inner.lock().await;
        let s = map.get(id)?;
        Some((s.events.clone(), s.bus.subscribe()))
    }

    pub async fn push_event(&self, event: SessionEvent) {
        let mut map = self.inner.lock().await;
        if let Some(s) = map.get_mut(&event.session_id) {
            s.events.push(event.clone());
            s.record.event_count = s.events.len();
            let _ = s.bus.send(event);
        }
    }

    pub async fn set_status(&self, id: &str, status: SessionStatus) {
        let mut map = self.inner.lock().await;
        if let Some(s) = map.get_mut(id) {
            s.record.status = status;
            if status.is_terminal() {
                s.record.finished_at = Some(chrono::Utc::now());
            }
        }
    }

    pub async fn finish(&self, id: &str, status: SessionStatus, result: GoalResult) {
        let mut map = self.inner.lock().await;
        if let Some(s) = map.get_mut(id) {
            s.record.status = status;
            s.record.result = Some(result);
            s.record.finished_at = Some(chrono::Utc::now());
        }
    }
}
