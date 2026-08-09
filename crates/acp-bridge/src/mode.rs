//! Liberado-owned ACP agent modes (coding pack / pure chat / face-via-daemon).
//!
//! Paseo registers **one** provider (`liberado-acp`). Mode selection is ACP
//! `session/set_mode` (and process default via `--mode` / `LIBERADO_ACP_MODE`),
//! not three different launch commands.

use serde_json::{Value, json};

/// Which Liberado engine an ACP session uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMode {
    /// Full coding pack (`LiberadoLoopBackend` + durable worktrees).
    Coding,
    /// In-process multi-turn chat (no vault/delegate; pure conversation).
    Chat,
    /// Face / human-interface path against a running `liberado serve` daemon.
    Face,
}

impl AgentMode {
    pub const ALL: [AgentMode; 3] = [AgentMode::Coding, AgentMode::Chat, AgentMode::Face];

    pub fn id(self) -> &'static str {
        match self {
            Self::Coding => "coding",
            Self::Chat => "chat",
            Self::Face => "face",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Coding => "Coding pack",
            Self::Chat => "Chat",
            Self::Face => "Face agent",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Coding => {
                "Full Liberado coding pack: worktrees, tools, progress/gate, traces (like Claude Code)"
            }
            Self::Chat => {
                "In-process conversational chat (no coding pack, no daemon). Multi-turn Q&A."
            }
            Self::Face => {
                "Daemon face agent: vault tools + delegate (requires liberado serve; LIBERADO_SERVER)"
            }
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "coding" | "code" | "coder" | "pack" => Some(Self::Coding),
            "chat" | "talk" | "conversation" => Some(Self::Chat),
            "face" | "delegate" | "main" | "daemon" => Some(Self::Face),
            _ => None,
        }
    }

    /// Process default: `--mode` / `LIBERADO_ACP_MODE`, else coding.
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
mod tests {
    use super::*;

    #[test]
    fn parse_aliases() {
        assert_eq!(AgentMode::parse("coding"), Some(AgentMode::Coding));
        assert_eq!(AgentMode::parse("CODE"), Some(AgentMode::Coding));
        assert_eq!(AgentMode::parse("chat"), Some(AgentMode::Chat));
        assert_eq!(AgentMode::parse("face"), Some(AgentMode::Face));
        assert_eq!(AgentMode::parse("delegate"), Some(AgentMode::Face));
        assert_eq!(AgentMode::parse("nope"), None);
    }

    #[test]
    fn mode_state_lists_all_three() {
        let v = mode_state_json(AgentMode::Chat);
        assert_eq!(v["currentModeId"], "chat");
        assert_eq!(v["availableModes"].as_array().unwrap().len(), 3);
    }
}
