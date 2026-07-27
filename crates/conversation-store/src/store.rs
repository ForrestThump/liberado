//! The [`ConversationStore`] trait — the durable decision (spec §5), distinct from the engine.
//!
//! Everything beyond the §3 schema is swappable behind this boundary: the v1 JSONL impl can give
//! way to SQLite or Postgres with no schema change. The trait is small on purpose — the hot path
//! ([`leaf_path`](ConversationStore::leaf_path)) and the control-plane random access
//! ([`node`](ConversationStore::node)) are the only reads orchestration actually needs.

use ulid::Ulid;

use crate::error::StoreResult;
use crate::types::{ConversationHeader, MessageNode, NewConversation, NewNode};

/// A persistent store of conversation logs. Implementors must honor the concurrency contract:
/// appends to one conversation are serialized and atomic (no torn or interleaved writes), different
/// conversations never contend, and a node is persisted only once complete.
#[async_trait::async_trait]
pub trait ConversationStore: Send + Sync {
    /// Create a new, empty conversation, minting its id and header. Returns the persisted header.
    async fn create(&self, new: NewConversation) -> StoreResult<ConversationHeader>;

    /// Append one COMPLETE node, minting its id at append time. Serialized per conversation, so the
    /// log stays sorted by id. Returns the persisted node (with its minted id and timestamp).
    async fn append(&self, conversation: Ulid, node: NewNode) -> StoreResult<MessageNode>;

    /// The leaf path root -> leaf, in order. `leaf = None` means the conversation's current leaf
    /// (the greatest id, i.e. the last appended node). This is what the executor consumes.
    async fn leaf_path(
        &self,
        conversation: Ulid,
        leaf: Option<Ulid>,
    ) -> StoreResult<Vec<MessageNode>>;

    /// Structural random access: fetch one node by id, regardless of whether it is in any prompt
    /// window. `Ok(None)` if the conversation exists but has no such node.
    async fn node(&self, conversation: Ulid, id: Ulid) -> StoreResult<Option<MessageNode>>;

    /// The ids of the direct children of `id` — the branch points — sorted ascending.
    async fn children(&self, conversation: Ulid, id: Ulid) -> StoreResult<Vec<Ulid>>;

    /// All conversation headers, newest first (by id).
    async fn list(&self) -> StoreResult<Vec<ConversationHeader>>;

    /// Read one conversation's header (line 0 of its log). `NotFound` if absent.
    async fn header(&self, conversation: Ulid) -> StoreResult<ConversationHeader>;

    /// Overwrite the title of an existing conversation. Idempotent across calls.
    /// Used by: first-message default, future flash-title agent, `PATCH /api/conversations/{id}`,
    /// and a future `/title` slash command. Titles are display-only and always overwritable.
    async fn set_title(&self, conversation: Ulid, title: String) -> StoreResult<()>;

    /// Permanently remove a conversation and every node in it.
    ///
    /// **Irreversible, and deliberately so.** There is no archive or soft-delete tier to fall back
    /// on, so this deletes the log rather than flagging it: nothing replays it on the next boot and
    /// no caller can resurrect it. A store that only hid the conversation would leave the bytes on
    /// disk while telling the user they were gone, which is the one thing "delete" must not mean.
    ///
    /// `NotFound` when it does not exist — the same contract as [`set_title`](Self::set_title), so a
    /// second delete is reported rather than silently succeeding. Callers wanting idempotent HTTP
    /// semantics map that themselves.
    async fn delete(&self, conversation: Ulid) -> StoreResult<()>;
}
