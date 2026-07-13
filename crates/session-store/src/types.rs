//! The converged `Session` schema (D7) — one header for chats and goal sessions alike.
//!
//! The whole content of D7 is that "conversation" and "goal session" were never two things. The
//! differences are **attributes, not subtypes**, and this struct is where that stops being a claim
//! and becomes a record on disk.

use liberado_conversation_store::{ConversationHeader, Timestamp};
use liberado_session::{
    GoalResult, GoalSessionRecord, GoalSpec, SessionGrant, SessionStatus, Visibility,
};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// One session — chat or goal, they are the same record.
///
/// **Terminality is `goal.is_some()`.** A session with a goal runs to a terminal status and then
/// stops; a goal-less session is a chat, which simply continues whenever a human says something
/// else. That single `Option` is the entire difference, and it is why there is one store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHeader {
    pub id: Ulid,
    /// Display title. Derived/regenerable, never the source of truth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// **The** distinguishing attribute (D7). `Some` ⇒ run-to-terminal goal session; `None` ⇒ chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalSpec>,

    /// The session this one came from — a **real edge**, not a stringly-typed cross-store
    /// reference. Before convergence this had to be a `String` in `SessionOrigin`, because the
    /// session kernel was forbidden from depending on the conversation store; the two lived in
    /// different id spaces and could only point at each other by name. One store, one id space, one
    /// `Ulid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<Ulid>,
    /// The specific node in the parent that spawned this session (a `delegate` call, a `/spawn`, a
    /// fork point). Together with `parent_session` this is what lets the session tree be walked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawned_by: Option<Ulid>,
    /// Ties this session to the dispatch journal entry that produced it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,

    #[serde(default)]
    pub visibility: Visibility,
    /// The authority this session runs under (S6) — resolved once at start, never widened.
    #[serde(default)]
    pub grant: SessionGrant,

    #[serde(default = "pending")]
    pub status: SessionStatus,
    pub created_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<GoalResult>,
    /// Derived from the event stream so a listing can badge a session that needs a human without
    /// scanning its log.
    #[serde(default)]
    pub awaiting_input: bool,
}

fn pending() -> SessionStatus {
    SessionStatus::Pending
}

impl SessionHeader {
    /// A goal-less session — a chat.
    pub fn chat(id: Ulid, title: Option<String>, created_at: Timestamp) -> Self {
        Self {
            id,
            title,
            goal: None,
            parent_session: None,
            spawned_by: None,
            correlation_id: None,
            visibility: Visibility::Foreground,
            grant: SessionGrant::default(),
            // A chat has no goal, so it is never "running toward" anything and never terminal.
            // `Running` is the honest status for something that is simply open.
            status: SessionStatus::Running,
            created_at,
            finished_at: None,
            result: None,
            awaiting_input: false,
        }
    }

    /// Whether this session runs to a terminal status (D7: terminality *is* `goal.is_some()`).
    pub fn is_terminal_kind(&self) -> bool {
        self.goal.is_some()
    }

    /// The chat lens onto this session. Every session has one — a goal session has a transcript
    /// too, which is exactly the point of converging.
    pub fn to_conversation_header(&self) -> ConversationHeader {
        ConversationHeader {
            id: self.id,
            title: self.title.clone().or_else(|| {
                // A goal session's "title" is its goal — so it reads sensibly in a session list
                // without anyone having to special-case it there.
                self.goal.as_ref().map(|g| g.description.clone())
            }),
            parent_conversation: self.parent_session,
            spawned_by: self.spawned_by,
            created_at: self.created_at,
        }
    }

    /// The kernel lens onto this session. `None` for a chat — the kernel's `GoalSessionRecord`
    /// cannot represent a goal-less session, which is precisely why the two views exist.
    pub fn to_goal_record(&self, event_count: usize) -> Option<GoalSessionRecord> {
        let goal = self.goal.clone()?;
        Some(GoalSessionRecord {
            id: self.id.to_string(),
            goal,
            grant: self.grant.clone(),
            visibility: self.visibility,
            status: self.status,
            created_at: self.created_at,
            finished_at: self.finished_at,
            result: self.result.clone(),
            event_count,
            awaiting_input: self.awaiting_input,
        })
    }
}

/// What a caller supplies to open a session; the store mints the id and timestamp.
#[derive(Debug, Clone, Default)]
pub struct NewSession {
    pub title: Option<String>,
    /// `Some` ⇒ a goal session. `None` ⇒ a chat.
    pub goal: Option<GoalSpec>,
    pub parent_session: Option<Ulid>,
    pub spawned_by: Option<Ulid>,
    pub correlation_id: Option<String>,
    pub visibility: Visibility,
    pub grant: SessionGrant,
}
