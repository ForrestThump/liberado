//! The kernel's own [`SessionRecordStore`] — an **in-memory double**, not the production store.
//!
//! Production runs on `liberado-session-store::SessionStore` (the converged store, D7). This exists
//! so the kernel's own tests — the hub, the life pack, background runs — can exercise a store
//! without depending on the `store` tier that sits above them. It is deliberately simple: it holds
//! turns as `(author, text)` rather than as a real message DAG, because the DAG is the *store's*
//! job and is tested there (`crates/session-store/tests/conversation_lens.rs`).
//!
//! Keep it that way. The lesson of 2026-07-13 (see `crates/conversation-store/src/lib.rs`) is that a
//! second *implementation* of a store, tested as if it were the real one, hides bugs in the real
//! one. A double that is obviously a double does not.
//!
//! When [`opened`](GoalSessionStore::open) with a directory it is also **durable**: each session is an append-only JSONL log
//! (`<dir>/<id>.jsonl`) — one `start` line (the initial record), one `event` line per session
//! event, and `status`/`finish` lines as the lifecycle advances. On boot, [`open`] replays every
//! log to rehydrate the list/snapshot views (session-focus S5). This mirrors the conversation
//! store's append-only-JSONL posture (Decision 12/17: operational data, outside the vault) rather
//! than introducing a database.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast};
use tracing::warn;

use crate::event::{SessionEvent, SessionEventKind};
use crate::goal::{GoalResult, GoalSessionRecord, SessionStatus, TerminalKind};
use crate::record_store::TurnAuthor;

const EVENT_CHANNEL_CAPACITY: usize = 256;

#[derive(Debug)]
struct SessionInner {
    record: GoalSessionRecord,
    events: Vec<SessionEvent>,
    /// Conversational turns, flat. The real store makes these message-DAG nodes; this double keeps
    /// them as `(who, what)` so kernel tests can assert *that a turn was recorded* without this
    /// crate having to know what a provider `Message` is.
    turns: Vec<(TurnAuthor, String)>,
    bus: broadcast::Sender<SessionEvent>,
}

/// One appended line of a session's durable log. Tagged so a replay can apply each in order.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum LogLine {
    /// The session was created — carries the initial record (goal spec, created_at, Pending).
    /// Boxed: it's much larger than the other variants (transient, so the extra alloc is per
    /// session-start only, and serde treats `Box<T>` transparently).
    Start { record: Box<GoalSessionRecord> },
    /// One session event, in emission order.
    Event { event: SessionEvent },
    /// A status transition (e.g. Pending → Running) with the terminal timestamp when set.
    Status {
        status: SessionStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finished_at: Option<DateTime<Utc>>,
    },
    /// The terminal record: final status, result, and finish time.
    Finish {
        status: SessionStatus,
        result: GoalResult,
        finished_at: DateTime<Utc>,
    },
}

/// Process-local session store, optionally backed by an append-only JSONL log per session.
/// Surfaces subscribe via [`subscribe`](Self::subscribe).
#[derive(Clone, Default)]
pub struct GoalSessionStore {
    inner: Arc<Mutex<HashMap<String, SessionInner>>>,
    /// When set, each mutation is appended to `<dir>/<id>.jsonl`. `None` = in-memory only.
    dir: Option<Arc<PathBuf>>,
}

impl GoalSessionStore {
    /// In-memory only (no persistence). Used by tests and ephemeral setups.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a **durable** store rooted at `dir`, rehydrating every `*.jsonl` session log found
    /// there. The directory is created if missing. A non-terminal session in a replayed log (a
    /// session that was mid-run when the daemon last stopped) is coerced to `Failed` — no pack is
    /// running it after a restart and packs aren't resumable yet, so its transcript is view-only.
    pub async fn open(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!(error = %e, path = %dir.display(), "goal-session store: could not create dir — continuing in-memory");
            return Self::new();
        }

        let mut map = HashMap::new();
        match std::fs::read_dir(&dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }
                    match replay_file(&path) {
                        Some(inner) => {
                            map.insert(inner.record.id.clone(), inner);
                        }
                        None => {
                            warn!(path = %path.display(), "goal-session store: skipped an unreadable log")
                        }
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, path = %dir.display(), "goal-session store: could not read dir")
            }
        }
        if !map.is_empty() {
            tracing::info!(
                sessions = map.len(),
                "goal-session store: rehydrated from disk"
            );
        }

        Self {
            inner: Arc::new(Mutex::new(map)),
            dir: Some(Arc::new(dir)),
        }
    }

    /// Append a line to a session's log (best-effort — a persistence failure is logged, never
    /// fatal to a running session). No-op for an in-memory store.
    fn append(&self, id: &str, line: &LogLine) {
        let Some(dir) = &self.dir else { return };
        let path = dir.join(format!("{}.jsonl", sanitize_id(id)));
        let mut serialized = match serde_json::to_string(line) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "goal-session store: serialize failed");
                return;
            }
        };
        serialized.push('\n');
        use std::io::Write;
        let write = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(serialized.as_bytes()));
        if let Err(e) = write {
            warn!(error = %e, path = %path.display(), "goal-session store: append failed");
        }
    }

    pub async fn insert(&self, record: GoalSessionRecord) {
        let id = record.id.clone();
        self.append(
            &id,
            &LogLine::Start {
                record: Box::new(record.clone()),
            },
        );
        let (bus, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let mut map = self.inner.lock().await;
        map.insert(
            id,
            SessionInner {
                record,
                events: Vec::new(),
                turns: Vec::new(),
                bus,
            },
        );
    }

    pub async fn get(&self, id: &str) -> Option<GoalSessionRecord> {
        self.inner.lock().await.get(id).map(|s| s.record.clone())
    }

    pub async fn list(&self) -> Vec<GoalSessionRecord> {
        let mut rows: Vec<_> = self
            .inner
            .lock()
            .await
            .values()
            .map(|s| s.record.clone())
            .collect();
        rows.sort_by_key(|row| std::cmp::Reverse(row.created_at));
        rows
    }

    pub async fn events(&self, id: &str) -> Option<Vec<SessionEvent>> {
        self.inner.lock().await.get(id).map(|s| s.events.clone())
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

    /// The turns recorded for `id`, in order — what a pack (or the hub) said and was told.
    pub async fn turns(&self, id: &str) -> Vec<(TurnAuthor, String)> {
        self.inner
            .lock()
            .await
            .get(id)
            .map(|s| s.turns.clone())
            .unwrap_or_default()
    }

    pub async fn append_turn(&self, session_id: &str, author: TurnAuthor, content: String) {
        if let Some(s) = self.inner.lock().await.get_mut(session_id) {
            s.turns.push((author, content));
        }
    }

    pub async fn push_event(&self, event: SessionEvent) {
        let persist = {
            let mut map = self.inner.lock().await;
            if let Some(s) = map.get_mut(&event.session_id) {
                match &event.kind {
                    SessionEventKind::AwaitingInput { .. } => s.record.awaiting_input = true,
                    SessionEventKind::HumanInput { .. } => s.record.awaiting_input = false,
                    _ => {}
                }
                s.events.push(event.clone());
                s.record.event_count = s.events.len();
                let _ = s.bus.send(event.clone());
                true
            } else {
                false
            }
        };
        if persist {
            let id = event.session_id.clone();
            self.append(&id, &LogLine::Event { event });
        }
    }

    pub async fn set_status(&self, id: &str, status: SessionStatus) {
        let finished_at = {
            let mut map = self.inner.lock().await;
            match map.get_mut(id) {
                Some(s) => {
                    s.record.status = status;
                    if status.is_terminal() {
                        s.record.finished_at = Some(Utc::now());
                    }
                    Some(s.record.finished_at)
                }
                None => None,
            }
        };
        if let Some(finished_at) = finished_at {
            self.append(
                id,
                &LogLine::Status {
                    status,
                    finished_at,
                },
            );
        }
    }

    pub async fn finish(&self, id: &str, status: SessionStatus, result: GoalResult) {
        let finished_at = {
            let mut map = self.inner.lock().await;
            match map.get_mut(id) {
                Some(s) => {
                    let finished_at = Utc::now();
                    s.record.status = status;
                    s.record.result = Some(result.clone());
                    s.record.finished_at = Some(finished_at);
                    // A terminal session cannot be awaiting input, even if it died mid-prompt
                    // (e.g. idle-budget BudgetExhausted after an AwaitingInput with no answer).
                    s.record.awaiting_input = false;
                    Some(finished_at)
                }
                None => None,
            }
        };
        if let Some(finished_at) = finished_at {
            self.append(
                id,
                &LogLine::Finish {
                    status,
                    result,
                    finished_at,
                },
            );
        }
    }
}

/// Replay one session log into a [`SessionInner`], applying lines in order. Returns `None` if the
/// file has no `start` line (nothing to anchor a record on).
fn replay_file(path: &Path) -> Option<SessionInner> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut record: Option<GoalSessionRecord> = None;
    let mut events: Vec<SessionEvent> = Vec::new();

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<LogLine>(line) else {
            continue; // tolerate a torn last line / unknown future line
        };
        match entry {
            LogLine::Start { record: r } => record = Some(*r),
            LogLine::Event { event } => {
                if let Some(rec) = record.as_mut() {
                    match &event.kind {
                        SessionEventKind::AwaitingInput { .. } => rec.awaiting_input = true,
                        SessionEventKind::HumanInput { .. } => rec.awaiting_input = false,
                        _ => {}
                    }
                }
                events.push(event);
            }
            LogLine::Status {
                status,
                finished_at,
            } => {
                if let Some(rec) = record.as_mut() {
                    rec.status = status;
                    rec.finished_at = finished_at;
                }
            }
            LogLine::Finish {
                status,
                result,
                finished_at,
            } => {
                if let Some(rec) = record.as_mut() {
                    rec.status = status;
                    rec.result = Some(result);
                    rec.finished_at = Some(finished_at);
                    rec.awaiting_input = false;
                }
            }
        }
    }

    let mut record = record?;
    record.event_count = events.len();

    // A session that was still running when the daemon stopped can't be resumed — mark it Failed so
    // surfaces show it as terminal (its transcript stays fully viewable on rejoin). E6 exception:
    // an awaiting session is only *parked*, not failed.
    if !record.status.is_terminal() {
        if record.awaiting_input {
            record.status = SessionStatus::Parked;
        } else {
            record.status = SessionStatus::Failed;
            record.awaiting_input = false;
            if record.result.is_none() {
                record.result = Some(GoalResult {
                    terminal: TerminalKind::Failed,
                    summary: "interrupted by daemon restart (not resumable)".into(),
                    artifacts: vec![],
                    diagnostics: serde_json::json!({ "interrupted": true }),
                });
            }
        }
    }

    let (bus, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
    Some(SessionInner {
        record,
        events,
        // This double does not persist turns — the production store does, as message-DAG nodes.
        turns: Vec::new(),
        bus,
    })
}

/// Make a session id safe as a filename (ULIDs already are; a client-supplied id might not be).
fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The kernel's view of this store (S5′). Delegates to the inherent methods above — keeping those
/// public means the concrete store stays directly usable (tests, tools) while the hub talks only to
/// the trait, so a converged `Session` store can take its place without the hub noticing.
#[async_trait::async_trait]
impl crate::record_store::SessionRecordStore for GoalSessionStore {
    async fn insert(&self, record: GoalSessionRecord) {
        GoalSessionStore::insert(self, record).await
    }
    async fn get(&self, id: &str) -> Option<GoalSessionRecord> {
        GoalSessionStore::get(self, id).await
    }
    async fn list(&self) -> Vec<GoalSessionRecord> {
        GoalSessionStore::list(self).await
    }
    async fn events(&self, id: &str) -> Option<Vec<SessionEvent>> {
        GoalSessionStore::events(self, id).await
    }
    async fn subscribe(
        &self,
        id: &str,
    ) -> Option<(Vec<SessionEvent>, broadcast::Receiver<SessionEvent>)> {
        GoalSessionStore::subscribe(self, id).await
    }
    async fn push_event(&self, event: SessionEvent) {
        GoalSessionStore::push_event(self, event).await
    }
    async fn live_subscriber_count(&self, id: &str) -> usize {
        let map = self.inner.lock().await;
        map.get(id).map(|s| s.bus.receiver_count()).unwrap_or(0)
    }
    async fn append_turn(&self, session_id: &str, author: TurnAuthor, content: String) {
        GoalSessionStore::append_turn(self, session_id, author, content).await
    }
    async fn turns(&self, session_id: &str) -> Vec<(TurnAuthor, String)> {
        GoalSessionStore::turns(self, session_id).await
    }
    async fn set_status(&self, id: &str, status: SessionStatus) {
        GoalSessionStore::set_status(self, id, status).await
    }
    async fn finish(&self, id: &str, status: SessionStatus, result: GoalResult) {
        GoalSessionStore::finish(self, id, status, result).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::{DomainHint, GoalSpec};

    fn record(id: &str, desc: &str) -> GoalSessionRecord {
        GoalSessionRecord::new(GoalSpec {
            id: Some(id.into()),
            description: desc.into(),
            success_criteria: vec![],
            domain: DomainHint::Life,
            max_turns: 0,
            max_idle_secs: None,
            origin: None,
            profile: None,
            payload: serde_json::json!({}),
        })
    }

    #[tokio::test]
    async fn durable_store_rehydrates_a_finished_session() {
        let dir = std::env::temp_dir().join(format!("liberado-goals-test-{}", ulid::Ulid::new()));
        {
            let store = GoalSessionStore::open(&dir).await;
            store.insert(record("s1", "capture a note")).await;
            store.set_status("s1", SessionStatus::Running).await;
            store
                .push_event(SessionEvent::new(
                    "s1",
                    SessionEventKind::AwaitingInput {
                        prompt: "title?".into(),
                        options: vec![],
                    },
                ))
                .await;
            store
                .push_event(SessionEvent::new(
                    "s1",
                    SessionEventKind::HumanInput {
                        text: "Weekly Review".into(),
                    },
                ))
                .await;
            store
                .finish(
                    "s1",
                    SessionStatus::Succeeded,
                    GoalResult {
                        terminal: TerminalKind::Succeeded,
                        summary: "wrote note".into(),
                        artifacts: vec!["vault/x.md".into()],
                        diagnostics: serde_json::json!({}),
                    },
                )
                .await;
        }

        // Reopen: a fresh store rehydrates purely from disk.
        let reopened = GoalSessionStore::open(&dir).await;
        let rec = reopened.get("s1").await.expect("session should rehydrate");
        assert_eq!(rec.status, SessionStatus::Succeeded);
        assert!(!rec.awaiting_input);
        assert_eq!(rec.result.as_ref().unwrap().summary, "wrote note");
        let events = reopened.events("s1").await.unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|e| matches!(
            &e.kind, SessionEventKind::HumanInput { text } if text == "Weekly Review"
        )));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn awaiting_session_is_parked_on_rehydrate_not_failed() {
        // E6: a session parked on a human must survive a restart as Parked — not Failed with the
        // question erased. Mid-execution (no awaiting_input) still fails; see the next test.
        let dir = std::env::temp_dir().join(format!("liberado-goals-test-{}", ulid::Ulid::new()));
        {
            let store = GoalSessionStore::open(&dir).await;
            store.insert(record("s2", "waiting on you")).await;
            store.set_status("s2", SessionStatus::Running).await;
            store
                .push_event(SessionEvent::new(
                    "s2",
                    SessionEventKind::AwaitingInput {
                        prompt: "still waiting".into(),
                        options: vec![],
                    },
                ))
                .await;
            // No finish — simulate a crash while awaiting input.
        }

        let reopened = GoalSessionStore::open(&dir).await;
        let rec = reopened.get("s2").await.unwrap();
        assert_eq!(rec.status, SessionStatus::Parked);
        assert!(
            rec.awaiting_input,
            "the open question must still be visible after restart"
        );
        assert!(rec.result.is_none(), "parked is not a terminal outcome");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn mid_execution_session_is_coerced_to_failed_on_rehydrate() {
        let dir = std::env::temp_dir().join(format!("liberado-goals-test-{}", ulid::Ulid::new()));
        {
            let store = GoalSessionStore::open(&dir).await;
            store.insert(record("s2b", "mid-build")).await;
            store.set_status("s2b", SessionStatus::Running).await;
            store
                .push_event(SessionEvent::new(
                    "s2b",
                    SessionEventKind::Progress {
                        message: "writing files".into(),
                    },
                ))
                .await;
            // No finish — simulate a crash mid-build (not awaiting).
        }

        let reopened = GoalSessionStore::open(&dir).await;
        let rec = reopened.get("s2b").await.unwrap();
        assert_eq!(rec.status, SessionStatus::Failed);
        assert!(!rec.awaiting_input);
        assert!(
            rec.result.as_ref().unwrap().summary.contains("interrupted"),
            "should note the interruption"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn in_memory_store_persists_nothing() {
        // The default store has no dir — mutations must not touch the filesystem.
        let store = GoalSessionStore::new();
        store.insert(record("s3", "ephemeral")).await;
        store
            .push_event(SessionEvent::new(
                "s3",
                SessionEventKind::Progress {
                    message: "tick".into(),
                },
            ))
            .await;
        assert_eq!(store.events("s3").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn push_event_human_input_clears_awaiting_flag() {
        let store = GoalSessionStore::new();
        let id = "s4-awaiting-test";
        store.insert(record(id, "test")).await;
        store.set_status(id, SessionStatus::Running).await;

        store
            .push_event(SessionEvent::new(
                id,
                SessionEventKind::AwaitingInput {
                    prompt: "title?".into(),
                    options: vec![],
                },
            ))
            .await;
        assert!(store.get(id).await.unwrap().awaiting_input);

        store
            .push_event(SessionEvent::new(
                id,
                SessionEventKind::HumanInput {
                    text: "answer".into(),
                },
            ))
            .await;
        assert!(!store.get(id).await.unwrap().awaiting_input);
    }

    #[test]
    fn sanitize_id_preserves_allowed_chars() {
        assert_eq!(sanitize_id("abc123"), "abc123");
        assert_eq!(sanitize_id("a-b_c"), "a-b_c");
    }

    #[test]
    fn sanitize_id_replaces_disallowed_chars() {
        assert_eq!(sanitize_id("a/b.c"), "a_b_c");
        assert_eq!(sanitize_id("hello world"), "hello_world");
        assert_eq!(sanitize_id("a👋b"), "a_b");
    }
}
