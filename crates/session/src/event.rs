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
    Error {
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
