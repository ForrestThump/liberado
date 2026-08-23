//! The persisted schema: the one thing Decision 17 locks in now (spec §3).
//!
//! A conversation is a **DAG of message nodes**, not a `Vec<Message>` on disk — a linear chat is
//! the degenerate case where every node's parent is the previous leaf. Carrying `id` + `parent_id`
//! from day one is what makes branching / fork / loop-back / debate *additive* later instead of a
//! migration. The message body itself is reused verbatim from [`liberado_provider`] so there is
//! exactly one definition of message content in the system.

use liberado_provider::{Message, Role};
use liberado_session::SessionGrant;
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

/// [`Author::Named`] identity of a **context-compaction marker** node: the rolling summary that
/// stands in for the history before it. The kernel's chat loader resumes the model-visible view
/// from the latest one.
pub const COMPACTION_AUTHOR: &str = "compaction";

/// [`Author::Named`] identity of the **re-appended tail copies** that follow a compaction marker.
///
/// A compaction writes `[marker] → [verbatim copies of the last K turns]` so the model-visible
/// view is a contiguous suffix of the log. The originals of those copies are still on the log
/// *before* the marker, which makes these the only nodes that duplicate another node's content.
///
/// This constant lives here, in the store vocabulary, rather than with the kernel's compaction
/// logic, because it is a **read contract**: every reader that walks a raw leaf path to present or
/// count messages — rendered history, `Author::User` turn indexing for fork/rewind, search
/// indexing — must skip these or it double-counts. See [`Author::is_compaction_tail_copy`].
pub const COMPACTION_TAIL_AUTHOR: &str = "compaction-tail";

impl Author {
    /// Whether this is one of compaction's re-appended tail copies
    /// ([`COMPACTION_TAIL_AUTHOR`]) — content that already appears earlier on the log.
    ///
    /// Readers presenting or counting conversation messages should skip these. The
    /// *model-visible* view must not: the copies are exactly what makes it a contiguous suffix.
    pub fn is_compaction_tail_copy(&self) -> bool {
        matches!(self, Author::Named(name) if name == COMPACTION_TAIL_AUTHOR)
    }

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
    /// The model this node was dispatched to (on an [`Author::User`] node) or produced by (on an
    /// [`Author::Assistant`] one). `None` on system, tool, and named nodes, and on every node
    /// written before this field existed.
    ///
    /// # Why it lives here and not on the header
    ///
    /// This is the record of *which model a conversation is on*, and it is deliberately derived
    /// rather than declared: a `header.model` field would be a second statement of the same fact,
    /// free to drift from what actually ran (`failure-modes.md` §6). The log already says what
    /// happened; reading the model off it means there is nothing to keep in sync, and the answer is
    /// always what did happen rather than what someone intended.
    ///
    /// **It is not part of [`Message`], and must not become so.** The provider request is built
    /// from `Message` alone, so a stamp here never reaches the model — which is right: a model
    /// being told which model wrote each earlier turn is noise at best, and an invitation to
    /// reason about its own identity at worst.
    ///
    /// # Reading it back
    ///
    /// Filter on [`Author`], never on `message.role`. Several things carry an assistant-role body
    /// without being the chat model — subagent handoffs are authored `goal-session`, compaction
    /// markers and tail copies are [`Author::Named`] — so a `role == Assistant` scan would migrate
    /// a conversation onto whichever model a delegation happened to use.
    ///
    /// # Not recorded
    ///
    /// The *provider* behind the model. `Provider` exposes no identity beyond its base URL, and
    /// threading the configured name through construction is a wider change than the routing this
    /// enables. Model ids here are already provider-namespaced (`z-ai/glm-4.5-air`); add the
    /// provider when two backends serve overlapping ids, which is when it starts to matter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
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
    /// **The authority this conversation runs under** — its session profile, resolved.
    ///
    /// On the chat lens because D7 says a conversation *is* a session, and authority is a session
    /// attribute; `SessionHeader.grant` has carried this for goal sessions all along, while chats
    /// were stamped with an empty default nobody read.
    ///
    /// An empty `capabilities` with `profile: None` means "no profile chosen" — the reader falls
    /// back to the process-wide grant, which is every conversation that existed before profiles.
    /// A *named* profile is authoritative even if its capability set is empty, so "this chat may
    /// call nothing" is expressible and distinguishable from "unset".
    #[serde(default)]
    pub grant: SessionGrant,
}

/// The input to [`create`](crate::ConversationStore::create): the caller supplies only intent, not
/// identity. The store mints the conversation id and stamps the time, so the *only* writer of ids
/// is the store (the property that keeps the log sorted).
#[derive(Debug, Clone)]
pub struct NewConversation {
    pub title: Option<String>,
    pub parent_conversation: Option<Ulid>,
    pub spawned_by: Option<Ulid>,
    /// **Incognito**: open this conversation in RAM only, so nothing about it is ever written to
    /// disk and it never appears in a listing. A store with no durable tier at all may ignore this —
    /// it is already telling the truth.
    pub ephemeral: bool,
    /// Whether a human is attending this chat ([`Visibility::Foreground`](liberado_session::Visibility::Foreground))
    /// or it is machinery / suite residue
    /// ([`Visibility::Background`](liberado_session::Visibility::Background)). Background chats are
    /// durable but filtered out of the sidebar (`list` skips them); defaults to foreground so every
    /// existing `create` call site keeps today's behaviour.
    pub visibility: liberado_session::Visibility,
    /// The session profile this conversation runs under, already resolved to capabilities by the
    /// caller. `Default` (empty, unnamed) means "no profile" — see [`ConversationHeader::grant`].
    ///
    /// Resolved by the caller, not here: turning a profile *name* into a capability set is a config
    /// question (`Config::resolve_session_profile`), and a store that reached for config would put
    /// the whole config stack underneath the storage layer.
    pub grant: SessionGrant,
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
    /// Which model this node was dispatched to (on a `User` node) or produced by (on an
    /// `Assistant` one). See [`MessageNode::model`].
    pub model: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exactly the [`COMPACTION_TAIL_AUTHOR`] identity counts as a tail copy — not the marker
    /// itself (`COMPACTION_AUTHOR`), not any role author, not look-alike names. Readers rely on
    /// this to skip duplicated content without dropping real messages.
    #[test]
    fn only_the_compaction_tail_author_is_a_tail_copy() {
        assert!(Author::Named(COMPACTION_TAIL_AUTHOR.into()).is_compaction_tail_copy());

        let not_copies = [
            Author::System,
            Author::User,
            Author::Assistant,
            Author::Tool,
            // The compaction *marker* precedes the copies and is model-visible on its own;
            // conflating it with the tail would hide the summary from readers that skip copies.
            Author::Named(COMPACTION_AUTHOR.into()),
            Author::Named("user".into()),
        ];
        for author in not_copies {
            assert!(
                !author.is_compaction_tail_copy(),
                "{author:?} is not a tail copy"
            );
        }
    }
}
