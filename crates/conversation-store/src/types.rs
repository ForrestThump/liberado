//! The persisted schema: the one thing Decision 17 locks in now (spec §3).
//!
//! A conversation is a **DAG of message nodes**, not a `Vec<Message>` on disk — a linear chat is
//! the degenerate case where every node's parent is the previous leaf. Carrying `id` + `parent_id`
//! from day one is what makes branching / fork / loop-back / debate *additive* later instead of a
//! migration. The message body itself is reused verbatim from [`liberado_provider`] so there is
//! exactly one definition of message content in the system.

use liberado_provider::{Message, Role};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// A wall-clock timestamp, UTC. An alias so the schema reads in domain terms and every record
/// stamps time the same way.
pub type Timestamp = chrono::DateTime<chrono::Utc>;

/// Who produced a node — the *identity* behind a message, orthogonal to the provider [`Role`].
///
/// `Role` is the coarse protocol slot (`user`/`assistant`/`tool`); `Author` is who that slot
/// actually was: the human, a specific assistant model, a named subagent, a debate participant.
/// This is the seam that makes multi-agent and debate additive — the store records identity per
/// node even though v1 never mints a [`Named`](Author::Named) author itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    System,
    User,
    Assistant,
    Tool,
    /// A named participant (subagent, debate role). Never produced by [`Author::from_role`]; it
    /// exists so a caller that knows the identity can record it without a schema change.
    Named(String),
}

impl Author {
    /// The identity that corresponds one-to-one to a provider [`Role`]. The fallback any caller can
    /// use when all it has is the role; [`Author::Named`] is never produced here because a role
    /// carries no name.
    pub fn from_role(role: Role) -> Self {
        match role {
            Role::System => Author::System,
            Role::User => Author::User,
            Role::Assistant => Author::Assistant,
            Role::Tool => Author::Tool,
        }
    }
}

/// One persisted message — a node in the conversation DAG. Appended once, never mutated.
///
/// The [`id`](MessageNode::id) is time-sortable and minted by the store *at append time*, which is
/// load-bearing: because the single writer assigns ids in append order, the log is intrinsically
/// sorted by id (file order == id order). A [`parent_id`](MessageNode::parent_id) is therefore
/// always a *smaller* id than `id`, so a parent is always earlier in the log and traversal only
/// ever seeks backward.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageNode {
    /// Time-sortable id, minted at append time by the store. See the type doc for why this matters.
    pub id: Ulid,
    /// The node this one replies to. `None` = conversation root. Always a smaller id than `id`.
    pub parent_id: Option<Ulid>,
    /// The conversation this node belongs to.
    pub conversation_id: Ulid,
    /// Who produced this node — identity, distinct from the message's provider role.
    pub author: Author,
    /// When the store appended this node.
    pub created_at: Timestamp,
    /// The message body itself (role / content / tool_calls / tool_call_id), reused from
    /// [`liberado_provider`] so message content has a single definition.
    pub message: Message,
}

/// A conversation's header record — the first line of its log. Carries lineage so subagent trees
/// and fan-out are expressible without a schema change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationHeader {
    pub id: Ulid,
    /// A display title for the sidebar. Derived/regenerable; never the source of truth.
    ///
    /// **Default:** first line of the first user message (cheap, local).  
    /// **Future writers (any may overwrite):** cold-start flash agent, `PATCH` API,
    /// slash `/title` — all via [`set_title`](crate::ConversationStore::set_title).
    pub title: Option<String>,
    /// Set when this conversation was spawned by another (subagent dispatch, a branch promoted to
    /// its own conversation) — lets the agent tree be walked.
    pub parent_conversation: Option<Ulid>,
    /// The message node that spawned this conversation, when applicable.
    pub spawned_by: Option<Ulid>,
    pub created_at: Timestamp,
}

/// The input to [`create`](crate::ConversationStore::create): the caller supplies only intent, not
/// identity. The store mints the conversation id and stamps the time, so the *only* writer of ids
/// is the store (the property that keeps the log sorted).
#[derive(Debug, Clone)]
pub struct NewConversation {
    pub title: Option<String>,
    pub parent_conversation: Option<Ulid>,
    pub spawned_by: Option<Ulid>,
}

/// The input to [`append`](crate::ConversationStore::append): a complete message plus its place in
/// the DAG. The id and timestamp are minted by the store at append time, not supplied here — that
/// is what guarantees mint-order == write-order.
#[derive(Debug, Clone)]
pub struct NewNode {
    /// The node this replies to, or `None` for a conversation root.
    pub parent_id: Option<Ulid>,
    pub author: Author,
    pub message: Message,
}
