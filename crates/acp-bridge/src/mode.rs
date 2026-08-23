//! Liberado-owned ACP agent modes.
//!
//! Paseo registers **one** provider (`liberado-acp`). Mode selection is ACP
//! `session/set_mode` (and process default via `--mode` / `LIBERADO_ACP_MODE`),
//! not four different launch commands.
//!
//! Two coding drivers share the same pack pieces (tools, worktree, `[coder]` tuning).
//! They differ only in the outer loop:
//!
//! - [`AgentMode::Coding`] — interactive conversation (one prompt = one turn).
//! - [`AgentMode::Goal`] — unattended pack run (one prompt = one `/goal` to a terminal).

use serde_json::{Value, json};

struct ModeInfo {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    aliases: &'static [&'static str],
}

/// Order matches [`AgentMode`] discriminants (`#[repr(usize)]`).
const MODE_INFO: [ModeInfo; 4] = [
    ModeInfo {
        id: "coding",
        name: "Coding",
        description: "Interactive coding: conversation + tools on a durable worktree (like Claude Code)",
        aliases: &["coding", "code", "coder", "interactive"],
    },
    ModeInfo {
        id: "goal",
        name: "Goal",
        description: "One-shot /goal: coding pack runs to a terminal (intake, worker, gate, ship bar)",
        aliases: &["goal", "pack", "unattended", "oneshot", "one-shot"],
    },
    ModeInfo {
        id: "chat",
        name: "Chat",
        description: "In-process conversational chat (no coding tools, no daemon). Multi-turn Q&A.",
        aliases: &["chat", "talk", "conversation"],
    },
    ModeInfo {
        id: "face",
        name: "Face agent",
        description: "Daemon face agent: vault tools + delegate (requires liberado serve; LIBERADO_SERVER)",
        aliases: &["face", "delegate", "main", "daemon"],
    },
];

/// Which Liberado engine an ACP session uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum AgentMode {
    /// Interactive coding: lasting `Conversation` + coding tools on a durable worktree.
    Coding,
    /// One-shot coding pack (`LiberadoLoopBackend`) — the unattended `/goal` driver.
    Goal,
    /// In-process multi-turn chat (no vault/delegate; pure conversation).
    Chat,
    /// Face / human-interface path against a running `liberado serve` daemon.
    Face,
}

impl AgentMode {
    pub const ALL: [AgentMode; 4] = [
        AgentMode::Coding,
        AgentMode::Goal,
        AgentMode::Chat,
        AgentMode::Face,
    ];

    /// Help / error listing so CLI and JSON-RPC cannot drift from [`Self::parse`].
    pub const EXPECTED: &'static str = "coding|goal|chat|face";

    pub fn id(self) -> &'static str {
        self.info().id
    }

    pub fn name(self) -> &'static str {
        self.info().name
    }

    pub fn description(self) -> &'static str {
        self.info().description
    }

    fn info(self) -> &'static ModeInfo {
        &MODE_INFO[self as usize]
    }

    /// Conversation + executor (coding tools or none). Not a pack run and not the daemon face.
    pub fn is_converse(self) -> bool {
        matches!(self, Self::Coding | Self::Chat)
    }

    /// Interactive coding attaches [`liberado_coder_tools::CodingToolRuntime`].
    pub fn uses_coding_tools(self) -> bool {
        matches!(self, Self::Coding)
    }

    pub fn parse(s: &str) -> Option<Self> {
        let key = s.trim().to_ascii_lowercase();
        Self::ALL
            .iter()
            .copied()
            .find(|mode| mode.info().aliases.iter().any(|alias| *alias == key))
    }

    /// Process default: `--mode` / `LIBERADO_ACP_MODE`, else coding (interactive).
    pub fn from_env_or_default() -> Self {
        if let Ok(s) = std::env::var("LIBERADO_ACP_MODE")
            && let Some(m) = Self::parse(&s)
        {
            return m;
        }
        Self::Coding
    }
}

/// ACP `modes` object for `session/new` / set_mode responses.
pub fn mode_state_json(current: AgentMode) -> Value {
    let available: Vec<Value> = AgentMode::ALL
        .iter()
        .map(|m| {
            json!({
                "id": m.id(),
                "name": m.name(),
                "description": m.description(),
            })
        })
        .collect();
    json!({
        "availableModes": available,
        "currentModeId": current.id(),
    })
}

#[cfg(test)]
#[path = "mode_tests.rs"]
mod tests;
