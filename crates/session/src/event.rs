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
    /// The pack is blocked waiting for human input (interactive sessions). Surfaces render the
    /// prompt and route the input box to this session. `options` carries multiple-choice answers
    /// (e.g. intake clarifiers); empty = free text.
    AwaitingInput {
        prompt: String,
        #[serde(default)]
        options: Vec<String>,
    },
    /// A human input was accepted into the session — echoed into history by the hub so the
    /// transcript is complete and replayable regardless of pack behavior.
    HumanInput {
        text: String,
    },
    ValidationFinished {
        ok: bool,
        summary: String,
    },
    /// One reviewer's vote in the completion gate (`completion_gate`). Emitted per vote, as it is
    /// cast, so a surface can render the gate deliberating instead of waiting for a single verdict.
    ///
    /// `kind` is `gatekeeper` | `fresh` | `strategist`. `coerced` marks a vote the gate
    /// *substituted* because the reviewer failed — surfaces should show it differently from a
    /// genuine rejection, since it means "we could not get an opinion", not "the work is wrong".
    CriticVerdict {
        reviewer: String,
        kind: String,
        approved: bool,
        #[serde(default)]
        issues: Vec<String>,
        #[serde(default)]
        coerced: bool,
    },
    /// A file in the session's workspace was created, modified, or deleted. Surfaces accumulate
    /// these into a changed-file list; `change` is `added` | `modified` | `deleted`.
    ///
    /// Paths are **workspace-relative**. An absolute host path would leak the daemon's filesystem
    /// layout to every connected client and would not mean anything on a machine that is not the
    /// one running the pack.
    FileChanged {
        path: String,
        change: String,
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
