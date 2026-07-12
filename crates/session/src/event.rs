//! Session event envelope — shared surface vocabulary for coding + life packs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One event in a goal session stream (SSE / TUI / WebUI).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    pub session_id: String,
    pub at: DateTime<Utc>,
    #[serde(flatten)]
    pub kind: SessionEventKind,
}

/// Event variants — domain-neutral names; packs put detail in `detail` / previews.
///
/// This is the **one** event vocabulary (2026-07-11 convergence): goal sessions emit it through
/// the hub, and the chat stream maps the executor's in-process `AgentEvent` tap onto the same
/// tags at the server boundary — exactly how the coding pack maps `CoderEvent` here. The wire
/// mirror clients decode lives in `chat_client_contract::SessionEventKind`; the serde tags below
/// are the SSE `event:` names (plus chat's bare-payload `session` / `token` frames).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEventKind {
    SessionStarted {
        domain: String,
        description: String,
    },
    RoleStarted {
        role: String,
        model: String,
    },
    RoleFinished {
        role: String,
    },
    /// An incremental text delta of a role's answer — chat turns stream these; packs that
    /// surface model output live can too.
    Token {
        text: String,
    },
    ToolStarted {
        name: String,
        args_preview: String,
    },
    ToolFinished {
        name: String,
        ok: bool,
        result_preview: String,
    },
    Progress {
        message: String,
    },
    ValidationFinished {
        ok: bool,
        summary: String,
    },
    LoopGuard {
        guard: String,
        action: String,
    },
    SessionFinished {
        status: String,
        summary: String,
    },
    /// Hard error. Named (and tagged) `failed`, not `error`: browser `EventSource` reserves the
    /// `error` event name for its own connection errors, so the SSE name must avoid it and the
    /// serde tag stays identical to the SSE name.
    Failed {
        message: String,
    },
}

impl SessionEvent {
    pub fn new(session_id: impl Into<String>, kind: SessionEventKind) -> Self {
        Self {
            session_id: session_id.into(),
            at: Utc::now(),
            kind,
        }
    }
}
