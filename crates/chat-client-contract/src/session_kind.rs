//! Session identity for surfaces: the **kind** of a session (Primary chat, Coding, Life, …) and a
//! read-only header DTO for listing goal sessions (`GET /api/goals`).
//!
//! This is the client half of the unified-`Session` model (session-focus D7): a surface presents
//! the primary chat and every goal session as one list, each labeled by its [`SessionKind`] — the
//! "which agent am I talking to" chip — derived from the pack `domain`. Terminality is a property of
//! *having a goal*, not of the record type; open-ended chat ([`SessionKind::Primary`]) simply has no
//! goal and never finishes.
//!
//! Kept here (the shared wire crate) so the TUI, CLI, and WebUI derive the same labels rather than
//! each hard-coding domain → glyph.

use serde::{Deserialize, Serialize};

/// The pack `domain` a goal session runs under, mirroring `liberado_session::DomainHint`'s wire
/// form: **a plain string** — the pack's name (`"coding"`, `"life"`, or any other, which lands in
/// [`Custom`](Self::Custom)). Surfaces don't depend on `liberado-session` (a store-tier crate), so
/// this is the client-tier mirror they deserialize from `GET /api/goals`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DomainWire {
    #[default]
    Coding,
    Life,
    Custom(String),
}

impl DomainWire {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Coding => "coding",
            Self::Life => "life",
            Self::Custom(s) => s.as_str(),
        }
    }
}

impl From<&str> for DomainWire {
    fn from(s: &str) -> Self {
        match s {
            "coding" => Self::Coding,
            "life" => Self::Life,
            other => Self::Custom(other.to_string()),
        }
    }
}

impl Serialize for DomainWire {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DomainWire {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::from(String::deserialize(d)?.as_str()))
    }
}

/// What kind of session this is, for at-a-glance identity. Orthogonal to whether it has a goal:
/// this is *which loop/tool-grouping drives it*, not its lifecycle. Open-ended by convention:
/// adding a config "hat" (session-focus S6) that names a new domain lands here as [`Self::Custom`]
/// until it earns a first-class label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    /// The generalist chat — face agent + `delegate`. The goal-less session.
    Primary,
    /// Coding pack — coder tools, git, cargo, sandboxed workspace.
    Coding,
    /// Life-ops pack — vault + tasks, no coding tools.
    Life,
    /// An unrecognized pack domain (a future config hat before it has a first-class label).
    Custom,
}

impl SessionKind {
    /// The kind of a goal session, from its pack `domain`. (The primary chat is
    /// [`Self::Primary`], constructed by the surface — it isn't a goal session.)
    pub fn from_domain(domain: &DomainWire) -> Self {
        match domain {
            DomainWire::Coding => Self::Coding,
            DomainWire::Life => Self::Life,
            DomainWire::Custom(_) => Self::Custom,
        }
    }

    /// Human label for the chip / switcher row (e.g. "Coding").
    pub fn label(&self) -> &'static str {
        match self {
            Self::Primary => "Primary",
            Self::Coding => "Coding",
            Self::Life => "Life",
            Self::Custom => "Custom",
        }
    }

    /// Short uppercase tag for a compact colored chip (e.g. `[CODE]`). ASCII-only so it renders in
    /// any terminal; surfaces pick the color.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Primary => "CHAT",
            Self::Coding => "CODE",
            Self::Life => "LIFE",
            Self::Custom => "PACK",
        }
    }

    /// One-line description of the tool grouping this kind carries — the "what can this agent do"
    /// hint shown beside the label so the kind isn't just a name.
    pub fn tools_blurb(&self) -> &'static str {
        match self {
            Self::Primary => "chat + delegate",
            Self::Coding => "coder tools · git · cargo",
            Self::Life => "vault + tasks",
            Self::Custom => "custom pack",
        }
    }
}

/// Just the `description` + `domain` of a listed goal session — the subset of `GoalSpec` a surface
/// needs to label a row. Extra `GoalSpec` fields (success criteria, budgets, payload) are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct GoalHeaderSpec {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub domain: DomainWire,
}

/// The terminal summary of a finished goal session (subset of `GoalResult`), for the switcher row
/// and the joined-view header.
#[derive(Debug, Clone, Deserialize)]
pub struct GoalHeaderResult {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub artifacts: Vec<String>,
}

/// A goal session as a surface sees it in `GET /api/goals` — the read-only header for the unified
/// session switcher. Deserializes from `liberado_session::GoalSessionRecord`'s JSON (unknown fields
/// ignored), keeping surfaces off the store-tier crate.
#[derive(Debug, Clone, Deserialize)]
pub struct GoalSessionHeader {
    pub id: String,
    #[serde(default)]
    pub goal: Option<GoalHeaderSpec>,
    /// Lifecycle tag as a string (`"running"`, `"succeeded"`, …) — surfaces render/branch on it
    /// without importing the `SessionStatus` enum.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub awaiting_input: bool,
    #[serde(default)]
    pub result: Option<GoalHeaderResult>,
}

impl GoalSessionHeader {
    /// The kind chip for this session, from its pack domain.
    pub fn kind(&self) -> SessionKind {
        self.goal
            .as_ref()
            .map(|g| SessionKind::from_domain(&g.domain))
            .unwrap_or(SessionKind::Custom)
    }

    /// The goal description (empty when absent).
    pub fn description(&self) -> &str {
        self.goal.as_ref().map(|g| g.description.as_str()).unwrap_or("")
    }

    /// Whether this session has reached a terminal status (matches
    /// `liberado_session::SessionStatus::is_terminal`).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "succeeded" | "failed" | "cancelled" | "budget_exhausted"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_from_domain_maps_known_packs() {
        assert_eq!(SessionKind::from_domain(&DomainWire::Coding), SessionKind::Coding);
        assert_eq!(SessionKind::from_domain(&DomainWire::Life), SessionKind::Life);
        assert_eq!(
            SessionKind::from_domain(&DomainWire::Custom("research".into())),
            SessionKind::Custom
        );
    }

    #[test]
    fn domain_wire_matches_domain_hint_json() {
        // `liberado_session::DomainHint` serializes as a plain pack-name string — including an
        // unknown one, which must deserialize to `Custom` rather than erroring (S6: a caller may
        // name a profile it can't know isn't a pack; the server resolves which).
        assert_eq!(
            serde_json::from_value::<DomainWire>(serde_json::json!("research")).unwrap(),
            DomainWire::Custom("research".into())
        );
        assert_eq!(
            serde_json::to_value(DomainWire::Custom("research".into())).unwrap(),
            serde_json::json!("research")
        );
        assert_eq!(
            serde_json::from_value::<DomainWire>(serde_json::json!("coding")).unwrap(),
            DomainWire::Coding
        );
        assert_eq!(
            serde_json::from_value::<DomainWire>(serde_json::json!("life")).unwrap(),
            DomainWire::Life
        );
    }

    #[test]
    fn header_deserializes_from_a_goal_session_record() {
        // The exact JSON shape `GET /api/goals` returns (a serialized `GoalSessionRecord`).
        let json = serde_json::json!({
            "id": "g_01ABC",
            "goal": {
                "description": "build a hello CLI",
                "success_criteria": ["compiles"],
                "domain": "coding",
                "max_turns": 8,
                "payload": {}
            },
            "status": "running",
            "created_at": "2026-07-12T00:00:00Z",
            "event_count": 3,
            "awaiting_input": true
        });
        let h: GoalSessionHeader = serde_json::from_value(json).unwrap();
        assert_eq!(h.id, "g_01ABC");
        assert_eq!(h.kind(), SessionKind::Coding);
        assert_eq!(h.description(), "build a hello CLI");
        assert!(h.awaiting_input);
        assert!(!h.is_terminal());
        assert_eq!(h.kind().tag(), "CODE");
    }

    #[test]
    fn terminal_status_and_result_parse() {
        let json = serde_json::json!({
            "id": "g1",
            "goal": { "description": "note it", "domain": "life" },
            "status": "succeeded",
            "created_at": "2026-07-12T00:00:00Z",
            "awaiting_input": false,
            "result": { "terminal": "succeeded", "summary": "wrote note", "artifacts": ["vault/x.md"] }
        });
        let h: GoalSessionHeader = serde_json::from_value(json).unwrap();
        assert!(h.is_terminal());
        assert_eq!(h.kind(), SessionKind::Life);
        assert_eq!(h.result.as_ref().unwrap().summary, "wrote note");
    }
}
