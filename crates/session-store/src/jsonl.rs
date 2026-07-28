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
    Author, ConversationHeader, ConversationStore, MessageNode, NewConversation, NewNode,
    StoreError, StoreResult,
};
use liberado_provider::Message;
use liberado_session::{
    GoalResult, GoalSessionRecord, SessionEvent, SessionEventKind, SessionRecordStore,
    SessionStatus, TurnAuthor,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast};
use tracing::warn;
use ulid::Ulid;

use crate::types::{NewSession, SessionHeader};
use liberado_session::Visibility;

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
    /// Mints **monotonic** ULIDs. `Ulid::new()` is *not* monotonic: two ids minted in the same
    /// millisecond differ only in their random suffix, so either may sort higher. The store's whole
    /// ordering story rests on ids being time-sortable — `MessageNode.id`'s own doc promises
    /// `parent_id` is "always a smaller id" — and `leaf_path(conv, None)` finds the newest turn by
    /// taking the **largest** id. Sub-millisecond appends are not hypothetical: a tool loop writes
    /// the assistant node and its tool-result node back to back. With `Ulid::new()` that pair can
    /// invert, and `leaf_path` then walks from the wrong leaf — silently truncating a conversation.
    /// Found by the fork tests, which append a whole chat inside one millisecond.
    ids: Arc<std::sync::Mutex<ulid::Generator>>,
    /// Per-session append locks, held across mint-and-write. See [`write_lock_for`](Self::write_lock_for).
    write_locks: Arc<std::sync::Mutex<HashMap<Ulid, Arc<tokio::sync::Mutex<()>>>>>,
    /// Sessions opened **incognito**: RAM only, never a byte on disk.
    ///
    /// Held as an id set rather than read off each session's header because the one place it is
    /// consulted — [`path_for`](Self::path_for) — is synchronous and runs *after* the async map lock
    /// has been dropped (see [`append`](ConversationStore::append), which persists outside the lock
    /// on purpose). Threading the flag through all seven `append_line` call sites instead would work
    /// exactly until someone adds an eighth and forgets. One chokepoint, one check.
    ephemeral: Arc<std::sync::Mutex<std::collections::HashSet<Ulid>>>,
}

impl SessionStore {
    /// The next id, monotonically after every id this store has already minted. Falls back to a
    /// plain `Ulid::new()` only if the generator overflows (2^80 ids inside one millisecond), which
    /// cannot happen in practice — and is still a valid, merely non-monotonic, id if it did.
    fn next_id(&self) -> Ulid {
        self.ids
            .lock()
            .expect("id generator mutex poisoned")
            .generate()
            .unwrap_or_else(|_| Ulid::new())
    }
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
            ids: Arc::new(std::sync::Mutex::new(ulid::Generator::new())),
            write_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            ephemeral: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
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
            ids: Arc::new(std::sync::Mutex::new(ulid::Generator::new())),
            write_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            ephemeral: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        }
    }

    /// Where this session's log lives, or `None` if it has none.
    ///
    /// **This is the whole of incognito.** Every durable write in this file funnels through
    /// [`append_line`](Self::append_line), which funnels through here, so a session that answers
    /// `None` cannot reach the disk by any path — not its header, not a node, not an event, not a
    /// retitle. That is why incognito is a check here and not a parallel storage backend.
    ///
    /// It also means an incognito chat is invisible to `GET /api/conversations/search`, which greps
    /// these files directly rather than going through the store: there is no file to match, so the
    /// exclusion needs no filter anyone could forget to write.
    fn path_for(&self, id: Ulid) -> Option<PathBuf> {
        if self.is_ephemeral(id) {
            return None;
        }
        self.dir.as_ref().map(|d| d.join(format!("{id}.jsonl")))
    }

    fn is_ephemeral(&self, id: Ulid) -> bool {
        self.ephemeral
            .lock()
            .map(|set| set.contains(&id))
            .unwrap_or(false)
    }

    /// Best-effort append. A failed write is logged, never panics: losing the durable copy of an
    /// event must not take down a running session.
    ///
    /// **One line, one `write_all`.** This used to be `writeln!(f, "{line}")`, which goes through
    /// `write_fmt` and is free to issue *several* `write` syscalls for one line. Two threads
    /// appending to the same session's log could then interleave between those syscalls and produce
    /// a spliced, unparseable line — losing not just that record but, because `replay_file` fails the
    /// whole file on any bad line, **the entire session**. Building the line (newline included) into
    /// one buffer and issuing a single `write_all` to an `O_APPEND` handle is atomic for writes of
    /// this size, so concurrent appenders can interleave *between* lines but never *within* one.
    fn append_line(&self, id: Ulid, record: &Record) {
        let Some(path) = self.path_for(id) else {
            return;
        };
        let mut line = match serde_json::to_string(record) {
            Ok(l) => l,
            Err(e) => {
                warn!(error = %e, "session store: serialize failed");
                return;
            }
        };
        line.push('\n');
        let write = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
        if let Err(e) = write {
            warn!(error = %e, path = %path.display(), "session store: append failed");
        }
    }

    /// The per-session lock held across **mint-and-write**, so a node's id order and its line's file
    /// order cannot disagree (`liberado-conversation-store-spec.md` §3: file order == id order).
    ///
    /// Minting under the in-memory map lock is not enough on its own: the durable write happens after
    /// that lock is dropped, so two appends could mint `id1 < id2` and then race, landing `id2` in
    /// the file first. Different sessions take different locks and so never contend.
    fn write_lock_for(&self, id: Ulid) -> Arc<tokio::sync::Mutex<()>> {
        self.write_locks
            .lock()
            .expect("write-lock registry poisoned")
            .entry(id)
            .or_default()
            .clone()
    }

    /// Open a session (chat or goal). The one constructor — that is the point.
    pub async fn create_session(&self, new: NewSession) -> SessionHeader {
        let id = self.next_id();
        // Registered *before* the header is appended below. Doing it after would write line 0 of an
        // incognito session's log to disk and then never write line 1 — the worst of both modes.
        if let (true, Ok(mut set)) = (new.ephemeral, self.ephemeral.lock()) {
            set.insert(id);
        }
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
            ephemeral: new.ephemeral,
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
    ///
    /// Incognito sessions are **not** listed. A chat that leaves no trace but shows up in the
    /// sidebar is not incognito, and both surfaces' listings (`/api/conversations` and
    /// `/api/sessions`) reach the store through here — so the exclusion is stated once. Lookup by id
    /// still works: the surface that opened one has to be able to read it back.
    pub async fn list_sessions(&self) -> Vec<SessionHeader> {
        let mut rows: Vec<_> = self
            .inner
            .lock()
            .await
            .values()
            .filter(|l| !l.header.ephemeral)
            .map(|l| l.header.clone())
            .collect();
        rows.sort_by_key(|row| std::cmp::Reverse(row.id));
        rows
    }

    /// Drop every incognito session untouched for at least `idle`, returning how many went.
    ///
    /// The backstop for a client that never got to say goodbye — a closed laptop, a killed browser,
    /// a dropped network. The surface discards its own session on the way out (that is the fast
    /// path, and the one that runs almost every time); this exists so "almost" is not the whole
    /// story. Nothing here touches the disk, because by construction there is nothing there.
    ///
    /// Idleness is read from the **ULID timestamps** already in the log — the newest node's, or the
    /// session's own if it never got one — rather than a `last_touched` field. ULIDs embed their
    /// mint time to the millisecond and this store mints them monotonically, so the information was
    /// already there; a parallel clock would only be a second thing to keep in sync.
    pub async fn sweep_ephemeral(&self, idle: std::time::Duration) -> usize {
        let cutoff_ms =
            (Utc::now().timestamp_millis() as u64).saturating_sub(idle.as_millis() as u64);
        let stale: Vec<Ulid> = self
            .inner
            .lock()
            .await
            .values()
            .filter(|l| l.header.ephemeral)
            .filter(|l| {
                let last = l
                    .nodes
                    .iter()
                    .map(|n| n.id.timestamp_ms())
                    .max()
                    .unwrap_or_else(|| l.header.id.timestamp_ms());
                // `<=`, not `<`: the predicate is "untouched for *at least* `idle`", and with
                // millisecond resolution a strict compare makes `Duration::ZERO` mean "sweep
                // nothing" — the exact opposite of what a zero window reads as.
                last <= cutoff_ms
            })
            .map(|l| l.header.id)
            .collect();

        for id in &stale {
            // Goes through the normal delete so the write-lock registry and the ephemeral set are
            // pruned too; the file removal inside is a no-op for a session that never had a file.
            let _ = ConversationStore::delete(self, *id).await;
        }
        if !stale.is_empty() {
            tracing::info!(count = stale.len(), "swept idle incognito sessions");
        }
        stale.len()
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

    /// Fork `source` into a new session carrying the conversation **up to and including `at`**
    /// (`None` = its newest leaf, i.e. the whole thing).
    ///
    /// # Copy, not reference
    ///
    /// The prefix nodes are **copied** into the fork's own log, with fresh ids, re-parented onto
    /// each other. The alternative — leaving the fork empty and stitching the parent's nodes in at
    /// read time via `parent_session` — was rejected:
    ///
    /// * It would break the store's one real invariant: **a session's log is self-contained**. Line
    ///   0 is its header and every node it needs is in the file. That is what makes a log greppable
    ///   on its own (`chat-search` reads these files directly), replayable on its own, and
    ///   deletable without silently gutting some other session.
    /// * It would give *live* semantics, not snapshot: appending to the original later would
    ///   retroactively change what the fork had "started from" if the fork point ever moved, and a
    ///   read-time stitch has to re-walk a chain of ancestors on every single load.
    ///
    /// Copy gives snapshot semantics, which is what a fork *means*: the original can be continued
    /// afterwards and the fork will not notice. Forks are rare and transcripts are small; the cost
    /// is a few kilobytes, and the invariant it preserves is worth far more.
    ///
    /// Lineage is still recorded — `parent_session` = `source`, `spawned_by` = the node forked at —
    /// so the relationship stays visible even though the content stands alone.
    pub async fn fork_session(
        &self,
        source: Ulid,
        at: Option<Ulid>,
        title: Option<String>,
    ) -> StoreResult<SessionHeader> {
        // The exact context that existed at the fork point — the same walk `leaf_path` does for a
        // normal load, which is why "branch mid-conversation" needs no new traversal: the DAG could
        // always reconstruct the prefix before any node; nothing ever *asked* it to.
        let prefix = self.leaf_path(source, at).await?;
        if prefix.is_empty() {
            // Reachable, but no longer the ordinary case for a goal session: packs record their
            // dialogue as turns now (`append_turn`), so a coding session's intake Q&A *is* a node
            // prefix and forking it works. What lands here is a session that genuinely said nothing.
            return Err(StoreError::NotFound(format!(
                "session {source} has no transcript to fork — nothing was said in it"
            )));
        }
        let fork_point = prefix.last().expect("non-empty").id;

        let parent = self
            .session(source)
            .await
            .ok_or_else(|| StoreError::NotFound(format!("session {source}")))?;

        let header = self
            .create_session(NewSession {
                title: title.or_else(|| parent.title.clone()),
                // A fork is a chat you continue. It deliberately does **not** inherit the source's
                // `goal`: a goal session runs to a terminal status under a pack, and copying the
                // goal would produce a session claiming to be running toward something with no pack
                // running it. The transcript is what forks; the mandate does not.
                goal: None,
                parent_session: Some(source),
                spawned_by: Some(fork_point),
                correlation_id: None,
                // Whoever forked it is sitting right there looking at it.
                visibility: Visibility::Foreground,
                grant: parent.grant.clone(),
                // A fork of an incognito chat is incognito too, or forking would be a way to launder
                // a RAM-only transcript onto the disk.
                ephemeral: parent.ephemeral,
            })
            .await;

        // Re-parent as we copy: each copied node points at the *copy* of its parent, not the
        // original. Walking `prefix` root-ward means a node's parent is always already copied.
        let mut previous: Option<Ulid> = None;
        for node in &prefix {
            let copied = self
                .append(
                    header.id,
                    NewNode {
                        parent_id: previous,
                        author: node.author.clone(),
                        message: node.message.clone(),
                    },
                )
                .await?;
            previous = Some(copied.id);
        }

        Ok(header)
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
    //
    // E6 exception: a session that was *parked on a human* (`awaiting_input`) is not mid-computation
    // — it is waiting. Coercing it to Failed erased the fact that a question was open. Promote those
    // to `Parked` instead (still non-terminal, still answerable once a resume path is wired). A
    // mid-build session without `awaiting_input` still becomes Failed — packs are not resumable yet.
    if header.goal.is_some() && !header.status.is_terminal() {
        if header.awaiting_input {
            header.status = SessionStatus::Parked;
            // Keep awaiting_input true so the UI still shows "needs you".
        } else {
            header.status = SessionStatus::Failed;
            header.awaiting_input = false;
            if header.finished_at.is_none() {
                header.finished_at = Some(Utc::now());
            }
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
                correlation_id: None,
                visibility: Default::default(),
                grant: Default::default(),
                ephemeral: new.ephemeral,
            })
            .await;
        Ok(header.to_conversation_header())
    }

    async fn append(&self, conversation: Ulid, node: NewNode) -> StoreResult<MessageNode> {
        // Held across mint-and-write: without it the id is minted under the map lock but the durable
        // write happens after that lock is dropped, so two concurrent appends can mint `id1 < id2`
        // and then race — landing `id2` in the file first and breaking file-order == id-order.
        let write_lock = self.write_lock_for(conversation);
        let _writing = write_lock.lock().await;

        let mut map = self.inner.lock().await;
        let live = map
            .get_mut(&conversation)
            .ok_or_else(|| StoreError::NotFound(format!("session {conversation}")))?;

        let persisted = MessageNode {
            id: self.next_id(),
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

    async fn delete(&self, conversation: Ulid) -> StoreResult<()> {
        let mut map = self.inner.lock().await;
        if map.remove(&conversation).is_none() {
            return Err(StoreError::NotFound(format!("session {conversation}")));
        }
        drop(map);

        // Drop the per-session append lock as well, or every delete leaves a dead entry behind in a
        // map that is never otherwise pruned.
        if let Ok(mut locks) = self.write_locks.lock() {
            locks.remove(&conversation);
        }
        // Likewise the incognito registry, which is otherwise never pruned. It also has to happen
        // before `path_for` below, or that call would still answer `None` for an id whose session is
        // already gone — harmless today (there is no file either way) but a trap for anyone who
        // later reuses `path_for` to mean "where this session's log would live".
        if let Ok(mut set) = self.ephemeral.lock() {
            set.remove(&conversation);
        }

        // The log file IS the record — one `{id}.jsonl` per session, replayed at boot — so removing
        // it is what makes this a real delete rather than an in-memory illusion that comes back on
        // restart. Blocking fs call to match `append_line`, which writes the same files the same way.
        if let Some(path) = self.path_for(conversation) {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                // Already absent on disk: an in-memory-only store, or a log a human removed by hand.
                // The in-memory eviction above is the part that had to happen, so this is success.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(StoreError::Io(e)),
            }
        }
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
            // A goal session is durable by construction: it is run by a pack, reported on, and
            // resumed. Incognito is a property of a human sitting at a chat surface asking for it.
            ephemeral: false,
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
        rows.sort_by_key(|row| std::cmp::Reverse(row.created_at));
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

    async fn live_subscriber_count(&self, id: &str) -> usize {
        let Ok(ulid) = id.parse::<Ulid>() else {
            return 0;
        };
        let map = self.inner.lock().await;
        map.get(&ulid)
            .map(|live| live.bus.receiver_count())
            .unwrap_or(0)
    }

    /// A pack's turn becomes a **real node in the message DAG** — the same kind of node a chat turn
    /// is. That is the whole point: it makes a pack's dialogue searchable (`chat-search` matches
    /// message nodes) and makes a goal session forkable (forking copies a node prefix, and a flat
    /// event log has nothing to branch from).
    ///
    /// This is where the kernel's provider-agnostic `TurnAuthor` + text becomes a provider
    /// `Message`. The kernel does not know how to build one; the store does. Parenting onto the
    /// current leaf happens here too, so a pack never has to track its own leaf.
    async fn append_turn(&self, session_id: &str, author: TurnAuthor, content: String) {
        let Ok(ulid) = session_id.parse::<Ulid>() else {
            return;
        };
        let (author, message) = match author {
            TurnAuthor::System => (Author::System, Message::system(content)),
            TurnAuthor::User => (Author::User, Message::user(content)),
            TurnAuthor::Assistant => (Author::Assistant, Message::assistant(content)),
            // A pack's tool *output* recorded as a turn is content, not a provider tool-result (which
            // would need a `tool_call_id` the pack has no notion of). Keep the identity, and let the
            // body be an ordinary message so the transcript still replays to a model cleanly.
            TurnAuthor::Tool => (Author::Tool, Message::assistant(content)),
            TurnAuthor::Named(name) => (Author::Named(name), Message::assistant(content)),
        };

        // The newest node, so a pack's transcript is a straight line without the pack tracking it.
        let parent_id = {
            let map = self.inner.lock().await;
            map.get(&ulid)
                .and_then(|l| l.nodes.iter().max_by_key(|n| n.id).map(|n| n.id))
        };

        if let Err(e) = self
            .append(
                ulid,
                NewNode {
                    parent_id,
                    author,
                    message,
                },
            )
            .await
        {
            warn!(error = %e, session = %session_id, "session store: append_turn failed");
        }
    }

    /// The inverse of [`append_turn`](Self::append_turn): the transcript, in order.
    ///
    /// Walks the leaf path rather than the raw node list, so a **forked** session yields its own
    /// branch and not the original's — the two share a node prefix by copy, and reading the flat
    /// node set would blur them back together.
    async fn turns(&self, session_id: &str) -> Vec<(TurnAuthor, String)> {
        let Ok(ulid) = session_id.parse::<Ulid>() else {
            return Vec::new();
        };
        let Ok(nodes) = ConversationStore::leaf_path(self, ulid, None).await else {
            return Vec::new();
        };
        nodes
            .into_iter()
            // A compaction's re-appended tail copy repeats text that is already earlier on this
            // path as the original — emitting both would show the last kept turns twice.
            .filter(|n| !n.author.is_compaction_tail_copy())
            .filter_map(|n| {
                let author = match n.author {
                    Author::System => TurnAuthor::System,
                    Author::User => TurnAuthor::User,
                    Author::Assistant => TurnAuthor::Assistant,
                    Author::Tool => TurnAuthor::Tool,
                    Author::Named(name) => TurnAuthor::Named(name),
                };
                // A node whose message carries no text (a bare tool-call node) has nothing to say
                // and is not dialogue.
                let content = n.message.content.clone();
                (!content.trim().is_empty()).then_some((author, content))
            })
            .collect()
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
