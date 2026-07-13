//! The converged store engine: one append-only JSONL log per **session**.
//!
//! Line 0 is the [`SessionHeader`]; every later line is one of:
//!
//! * `node`   — a [`MessageNode`]: a provider-replayable turn, carrying `parent_id` (the DAG).
//! * `event`  — a [`SessionEvent`]: an observation from a pack (tool started, awaiting input, …).
//! * `status` / `finish` — lifecycle transitions for goal-bearing sessions.
//!
//! A chat's log is all `node` lines. A goal session's log is all `event` lines. An interactive
//! coding session (S7) emits **both** — its intake Q&A are turns, its tool calls are observations —
//! and that is the case that proves these were never two different things.
//!
//! Two typed views are served from this one log ([`ConversationStore`] and [`SessionRecordStore`])
//! because the kernel may not know what a provider `Message` is (see `record_store.rs`). One engine,
//! one id space, two lenses.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use liberado_conversation_store::{
    ConversationHeader, ConversationStore, MessageNode, NewConversation, NewNode, StoreError,
    StoreResult,
};
use liberado_session::{
    GoalResult, GoalSessionRecord, SessionEvent, SessionEventKind, SessionRecordStore,
    SessionStatus,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast};
use tracing::warn;
use ulid::Ulid;

use crate::types::{NewSession, SessionHeader};

const EVENT_CHANNEL_CAPACITY: usize = 256;

/// One line of a session log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    Header(Box<SessionHeader>),
    Node(MessageNode),
    Event(SessionEvent),
    Status {
        status: SessionStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finished_at: Option<chrono::DateTime<Utc>>,
    },
    Finish {
        status: SessionStatus,
        result: GoalResult,
        finished_at: chrono::DateTime<Utc>,
    },
}

#[derive(Debug)]
struct Live {
    header: SessionHeader,
    nodes: Vec<MessageNode>,
    events: Vec<SessionEvent>,
    bus: broadcast::Sender<SessionEvent>,
}

/// The converged `Session` store (D7). Durable when opened with a directory; in-memory otherwise.
#[derive(Clone)]
pub struct SessionStore {
    inner: Arc<Mutex<HashMap<Ulid, Live>>>,
    dir: Option<Arc<PathBuf>>,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStore {
    /// In-memory only — tests and ephemeral use.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            dir: None,
        }
    }

    /// Open a durable store rooted at `dir`, replaying every `*.jsonl` found there.
    ///
    /// A session that was mid-run when the daemon stopped is coerced to `Failed`: no pack is running
    /// it any more and packs are not resumable, so leaving it `Running` would be a lie the UI would
    /// faithfully render forever. Chats are goal-less and so are never coerced — an open chat is
    /// simply open.
    pub async fn open(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!(error = %e, path = %dir.display(), "session store: could not create dir; running in-memory");
            return Self::new();
        }

        let mut map: HashMap<Ulid, Live> = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                match replay_file(&path) {
                    Ok(Some(live)) => {
                        map.insert(live.header.id, live);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!(error = %e, path = %path.display(), "session store: replay failed")
                    }
                }
            }
        }
        tracing::info!(sessions = map.len(), dir = %dir.display(), "session store rehydrated");

        Self {
            inner: Arc::new(Mutex::new(map)),
            dir: Some(Arc::new(dir)),
        }
    }

    fn path_for(&self, id: Ulid) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(format!("{id}.jsonl")))
    }

    /// Best-effort append. A failed write is logged, never panics: losing the durable copy of an
    /// event must not take down a running session.
    fn append_line(&self, id: Ulid, record: &Record) {
        let Some(path) = self.path_for(id) else {
            return;
        };
        let line = match serde_json::to_string(record) {
            Ok(l) => l,
            Err(e) => {
                warn!(error = %e, "session store: serialize failed");
                return;
            }
        };
        let write = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = write {
            warn!(error = %e, path = %path.display(), "session store: append failed");
        }
    }

    /// Open a session (chat or goal). The one constructor — that is the point.
    pub async fn create_session(&self, new: NewSession) -> SessionHeader {
        let id = Ulid::new();
        let header = SessionHeader {
            id,
            title: new.title,
            // A goal-bearing session starts Pending (a pack will run it); a chat is simply open.
            status: if new.goal.is_some() {
                SessionStatus::Pending
            } else {
                SessionStatus::Running
            },
            goal: new.goal,
            parent_session: new.parent_session,
            spawned_by: new.spawned_by,
            correlation_id: new.correlation_id,
            visibility: new.visibility,
            grant: new.grant,
            created_at: Utc::now(),
            finished_at: None,
            result: None,
            awaiting_input: false,
        };
        self.append_line(id, &Record::Header(Box::new(header.clone())));
        let (bus, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        self.inner.lock().await.insert(
            id,
            Live {
                header: header.clone(),
                nodes: Vec::new(),
                events: Vec::new(),
                bus,
            },
        );
        header
    }

    /// Every session, newest first — chats and goal sessions in **one list**. This is the call the
    /// unified switcher wanted all along; before convergence it had to poll two endpoints and glue
    /// the results together in the client.
    pub async fn list_sessions(&self) -> Vec<SessionHeader> {
        let mut rows: Vec<_> = self
            .inner
            .lock()
            .await
            .values()
            .map(|l| l.header.clone())
            .collect();
        rows.sort_by(|a, b| b.id.cmp(&a.id));
        rows
    }

    pub async fn session(&self, id: Ulid) -> Option<SessionHeader> {
        self.inner.lock().await.get(&id).map(|l| l.header.clone())
    }

    /// The direct children of `session` — the sessions it spawned. The session tree, walkable.
    pub async fn children_of(&self, session: Ulid) -> Vec<SessionHeader> {
        let mut rows: Vec<_> = self
            .inner
            .lock()
            .await
            .values()
            .filter(|l| l.header.parent_session == Some(session))
            .map(|l| l.header.clone())
            .collect();
        rows.sort_by_key(|h| h.id);
        rows
    }
}

/// Replay one session log into memory.
fn replay_file(path: &Path) -> Result<Option<Live>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut header: Option<SessionHeader> = None;
    let mut nodes: Vec<MessageNode> = Vec::new();
    let mut events: Vec<SessionEvent> = Vec::new();

    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: Record =
            serde_json::from_str(line).map_err(|e| format!("line {}: {e}", i + 1))?;
        match record {
            Record::Header(h) => header = Some(*h),
            Record::Node(n) => nodes.push(n),
            Record::Event(e) => {
                if let Some(h) = header.as_mut() {
                    apply_event_to_header(h, &e.kind);
                }
                events.push(e);
            }
            Record::Status {
                status,
                finished_at,
            } => {
                if let Some(h) = header.as_mut() {
                    h.status = status;
                    h.finished_at = finished_at;
                }
            }
            Record::Finish {
                status,
                result,
                finished_at,
            } => {
                if let Some(h) = header.as_mut() {
                    h.status = status;
                    h.result = Some(result);
                    h.finished_at = Some(finished_at);
                    h.awaiting_input = false;
                }
            }
        }
    }

    let Some(mut header) = header else {
        return Ok(None);
    };

    // A goal session left non-terminal by a crash is coerced: nothing is running it now. A chat
    // (goal-less) has no terminal state to coerce to — it is just open.
    if header.goal.is_some() && !header.status.is_terminal() {
        header.status = SessionStatus::Failed;
        header.awaiting_input = false;
        if header.finished_at.is_none() {
            header.finished_at = Some(Utc::now());
        }
    }

    let (bus, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
    Ok(Some(Live {
        header,
        nodes,
        events,
        bus,
    }))
}

/// `awaiting_input` is derived from the event stream, not set by hand, so it can never drift from
/// the transcript that produced it.
fn apply_event_to_header(header: &mut SessionHeader, kind: &SessionEventKind) {
    match kind {
        SessionEventKind::AwaitingInput { .. } => header.awaiting_input = true,
        SessionEventKind::HumanInput { .. } => header.awaiting_input = false,
        _ => {}
    }
}

// ── Lens 1: the chat view (nodes + messages) ─────────────────────────────────────────────────

#[async_trait]
impl ConversationStore for SessionStore {
    async fn create(&self, new: NewConversation) -> StoreResult<ConversationHeader> {
        let header = self
            .create_session(NewSession {
                title: new.title,
                goal: None, // a conversation is a goal-less session — that is the whole of D7
                parent_session: new.parent_conversation,
                spawned_by: new.spawned_by,
                ..Default::default()
            })
            .await;
        Ok(header.to_conversation_header())
    }

    async fn append(&self, conversation: Ulid, node: NewNode) -> StoreResult<MessageNode> {
        let mut map = self.inner.lock().await;
        let live = map
            .get_mut(&conversation)
            .ok_or_else(|| StoreError::NotFound(format!("session {conversation}")))?;

        let persisted = MessageNode {
            id: Ulid::new(),
            parent_id: node.parent_id,
            conversation_id: conversation,
            author: node.author,
            created_at: Utc::now(),
            message: node.message,
        };
        live.nodes.push(persisted.clone());
        drop(map);

        self.append_line(conversation, &Record::Node(persisted.clone()));
        Ok(persisted)
    }

    async fn leaf_path(
        &self,
        conversation: Ulid,
        leaf: Option<Ulid>,
    ) -> StoreResult<Vec<MessageNode>> {
        let map = self.inner.lock().await;
        let live = map
            .get(&conversation)
            .ok_or_else(|| StoreError::NotFound(format!("session {conversation}")))?;

        let by_id: HashMap<Ulid, &MessageNode> = live.nodes.iter().map(|n| (n.id, n)).collect();
        let leaf_node = match leaf {
            Some(id) => match by_id.get(&id) {
                Some(n) => *n,
                None => {
                    return Err(StoreError::NotFound(format!(
                        "node {id} in session {conversation}"
                    )));
                }
            },
            None => match live.nodes.iter().max_by_key(|n| n.id) {
                Some(n) => n,
                None => return Ok(Vec::new()),
            },
        };

        // Walk root-ward, bounded by the node count so a malformed cycle cannot spin forever.
        let mut path = Vec::new();
        let mut current = Some(leaf_node);
        while let Some(node) = current {
            path.push(node.clone());
            if path.len() > live.nodes.len() {
                return Err(StoreError::Corrupt(format!(
                    "parent cycle in session {conversation}"
                )));
            }
            current = match node.parent_id {
                Some(pid) => Some(*by_id.get(&pid).ok_or_else(|| {
                    StoreError::Corrupt(format!(
                        "node {} in session {conversation} references missing parent {pid}",
                        node.id
                    ))
                })?),
                None => None,
            };
        }
        path.reverse();
        Ok(path)
    }

    async fn node(&self, conversation: Ulid, id: Ulid) -> StoreResult<Option<MessageNode>> {
        let map = self.inner.lock().await;
        let live = map
            .get(&conversation)
            .ok_or_else(|| StoreError::NotFound(format!("session {conversation}")))?;
        Ok(live.nodes.iter().find(|n| n.id == id).cloned())
    }

    async fn children(&self, conversation: Ulid, id: Ulid) -> StoreResult<Vec<Ulid>> {
        let map = self.inner.lock().await;
        let live = map
            .get(&conversation)
            .ok_or_else(|| StoreError::NotFound(format!("session {conversation}")))?;
        let mut kids: Vec<Ulid> = live
            .nodes
            .iter()
            .filter(|n| n.parent_id == Some(id))
            .map(|n| n.id)
            .collect();
        kids.sort();
        Ok(kids)
    }

    async fn list(&self) -> StoreResult<Vec<ConversationHeader>> {
        Ok(self
            .list_sessions()
            .await
            .iter()
            .map(SessionHeader::to_conversation_header)
            .collect())
    }

    async fn header(&self, conversation: Ulid) -> StoreResult<ConversationHeader> {
        self.session(conversation)
            .await
            .map(|h| h.to_conversation_header())
            .ok_or_else(|| StoreError::NotFound(format!("session {conversation}")))
    }

    async fn set_title(&self, conversation: Ulid, title: String) -> StoreResult<()> {
        let mut map = self.inner.lock().await;
        let live = map
            .get_mut(&conversation)
            .ok_or_else(|| StoreError::NotFound(format!("session {conversation}")))?;
        live.header.title = Some(title);
        let header = live.header.clone();
        drop(map);
        // The header is line 0 and the log is append-only, so a title change is a *new* header line.
        // Replay takes the last one it sees, which makes the rewrite idempotent and keeps the log's
        // one invariant (never mutate what was written) intact.
        self.append_line(conversation, &Record::Header(Box::new(header)));
        Ok(())
    }
}

// ── Lens 2: the kernel view (records + events) ───────────────────────────────────────────────

#[async_trait]
impl SessionRecordStore for SessionStore {
    async fn insert(&self, record: GoalSessionRecord) {
        // The kernel mints ids as `String`; honor it so a session keeps one identity end to end.
        let id = record.id.parse::<Ulid>().unwrap_or_else(|_| Ulid::new());
        let header = SessionHeader {
            id,
            title: None,
            goal: Some(record.goal.clone()),
            parent_session: record
                .goal
                .origin
                .as_ref()
                .and_then(|o| o.conversation_id.as_deref())
                .and_then(|c| c.parse::<Ulid>().ok()),
            spawned_by: None,
            correlation_id: record
                .goal
                .origin
                .as_ref()
                .and_then(|o| o.correlation_id.clone()),
            // Honor what the caller recorded. This was hardcoded `Foreground`, because the kernel's
            // record could not carry visibility at all — which is what made a background session
            // unrepresentable through the very lens every non-human trigger writes through.
            visibility: record.visibility,
            grant: record.grant.clone(),
            status: record.status,
            created_at: record.created_at,
            finished_at: record.finished_at,
            result: record.result.clone(),
            awaiting_input: record.awaiting_input,
        };
        self.append_line(id, &Record::Header(Box::new(header.clone())));
        let (bus, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        self.inner.lock().await.insert(
            id,
            Live {
                header,
                nodes: Vec::new(),
                events: Vec::new(),
                bus,
            },
        );
    }

    async fn get(&self, id: &str) -> Option<GoalSessionRecord> {
        let ulid = id.parse::<Ulid>().ok()?;
        let map = self.inner.lock().await;
        let live = map.get(&ulid)?;
        live.header.to_goal_record(live.events.len())
    }

    async fn list(&self) -> Vec<GoalSessionRecord> {
        let map = self.inner.lock().await;
        let mut rows: Vec<_> = map
            .values()
            // Goal-less sessions (chats) have no `GoalSessionRecord` representation — the kernel's
            // type simply cannot express them. They are still in the store; this lens just can't
            // see them, which is what the unified `list_sessions` is for.
            .filter_map(|l| l.header.to_goal_record(l.events.len()))
            .collect();
        rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        rows
    }

    async fn events(&self, id: &str) -> Option<Vec<SessionEvent>> {
        let ulid = id.parse::<Ulid>().ok()?;
        self.inner.lock().await.get(&ulid).map(|l| l.events.clone())
    }

    async fn subscribe(
        &self,
        id: &str,
    ) -> Option<(Vec<SessionEvent>, broadcast::Receiver<SessionEvent>)> {
        let ulid = id.parse::<Ulid>().ok()?;
        let map = self.inner.lock().await;
        let live = map.get(&ulid)?;
        Some((live.events.clone(), live.bus.subscribe()))
    }

    async fn push_event(&self, event: SessionEvent) {
        let Ok(ulid) = event.session_id.parse::<Ulid>() else {
            return;
        };
        let persist = {
            let mut map = self.inner.lock().await;
            match map.get_mut(&ulid) {
                Some(live) => {
                    apply_event_to_header(&mut live.header, &event.kind);
                    live.events.push(event.clone());
                    let _ = live.bus.send(event.clone());
                    true
                }
                None => false,
            }
        };
        if persist {
            self.append_line(ulid, &Record::Event(event));
        }
    }

    async fn set_status(&self, id: &str, status: SessionStatus) {
        let Ok(ulid) = id.parse::<Ulid>() else { return };
        let finished_at = {
            let mut map = self.inner.lock().await;
            match map.get_mut(&ulid) {
                Some(live) => {
                    live.header.status = status;
                    if status.is_terminal() {
                        live.header.finished_at = Some(Utc::now());
                    }
                    Some(live.header.finished_at)
                }
                None => None,
            }
        };
        if let Some(finished_at) = finished_at {
            self.append_line(
                ulid,
                &Record::Status {
                    status,
                    finished_at,
                },
            );
        }
    }

    async fn finish(&self, id: &str, status: SessionStatus, result: GoalResult) {
        let Ok(ulid) = id.parse::<Ulid>() else { return };
        let finished_at = {
            let mut map = self.inner.lock().await;
            match map.get_mut(&ulid) {
                Some(live) => {
                    let now = Utc::now();
                    live.header.status = status;
                    live.header.result = Some(result.clone());
                    live.header.finished_at = Some(now);
                    // Terminal sessions are never awaiting, even if they died mid-prompt.
                    live.header.awaiting_input = false;
                    Some(now)
                }
                None => None,
            }
        };
        if let Some(finished_at) = finished_at {
            self.append_line(
                ulid,
                &Record::Finish {
                    status,
                    result,
                    finished_at,
                },
            );
        }
    }
}
