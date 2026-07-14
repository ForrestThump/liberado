//! Goal and session status types (domain-neutral).

use chrono::{DateTime, Utc};
use liberado_common::CapabilitySet;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Which domain pack should run this goal.
///
/// **Wire form is a plain string** — the pack's name (`"coding"`, `"life"`, or any other). A domain
/// *is* a pack name, so an unrecognized one deserializes to [`Custom`](Self::Custom) rather than
/// erroring: that is what lets a caller name a `[[session_profiles]]` hat (S6) it can't know is a
/// profile rather than a pack, and let the server resolve which. An unregistered name still fails
/// loudly at `start` ("no domain pack registered"), just not at the JSON boundary.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DomainHint {
    /// Default for dogfood / self-improvement coding goals.
    #[default]
    Coding,
    Life,
    /// Explicit pack name for future packs.
    Custom(String),
}

impl DomainHint {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Coding => "coding",
            Self::Life => "life",
            Self::Custom(s) => s.as_str(),
        }
    }
}

impl From<&str> for DomainHint {
    fn from(s: &str) -> Self {
        match s {
            "coding" => Self::Coding,
            "life" => Self::Life,
            other => Self::Custom(other.to_string()),
        }
    }
}

impl Serialize for DomainHint {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DomainHint {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::from(String::deserialize(d)?.as_str()))
    }
}

/// Where a session came from (session-focus D2/S4). Separate-but-linked: the session's transcript
/// stays its own, but on terminal its summary can be folded back into `conversation_id`, and
/// `correlation_id` stitches it to the dispatch journal — the same linkage `delegate` already uses.
/// `None` for sessions a human started directly (`POST /api/goals`, `/sessions` switcher).
///
/// **Both fields are optional, and independently so** — they are two different facts that happen to
/// arrive together for a chat-spawned session. A `delegate`d subagent has both. A **cron firing has
/// a correlation id and no parent conversation at all** (S5′ step 5): nobody spawned it from a chat,
/// yet it still belongs to a dispatch journal entry. Requiring a parent here would have forced every
/// unattended trigger to either invent one or throw its correlation away.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOrigin {
    /// The parent conversation id (a `conversation-store` ULID, as a string here to keep the
    /// session kernel off that store-tier crate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    /// The dispatch correlation id that ties this session to the journal entry that spawned it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

impl SessionOrigin {
    /// A session spawned by a chat turn — the S4 offer/return handoff.
    pub fn from_conversation(conversation_id: impl Into<String>) -> Self {
        Self {
            conversation_id: Some(conversation_id.into()),
            correlation_id: None,
        }
    }

    /// A session with a dispatch correlation but **no parent conversation** — a cron, a webhook.
    pub fn from_correlation(correlation_id: impl Into<String>) -> Self {
        Self {
            conversation_id: None,
            correlation_id: Some(correlation_id.into()),
        }
    }
}

/// Input to start a goal session (HTTP body / client contract).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalSpec {
    /// Optional client-supplied id; server generates a ULID when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub description: String,
    #[serde(default)]
    pub success_criteria: Vec<String>,
    #[serde(default)]
    pub domain: DomainHint,
    /// Soft turn budget for the pack (0 = pack default).
    #[serde(default)]
    pub max_turns: u32,
    /// Kernel idle budget for **interactive** sessions: if the pack blocks on human input this
    /// long with no answer, the session terminates `BudgetExhausted` (an abandoned session must
    /// still die). `None` = wait indefinitely. Non-interactive packs ignore it. This is a kernel
    /// budget, not a pack knob — enforced by [`InputChannel`](crate::InputChannel).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_idle_secs: Option<u64>,
    /// Set when a chat turn spawned this session — the link back to the parent conversation for the
    /// offer/return handoff (S4). `None` for human-started sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<SessionOrigin>,
    /// Which `[[session_profiles]]` "hat" to run this session under (S6) — selects the domain pack,
    /// the capability grant, and the pack's opaque overrides. `None` runs the bare `domain` with the
    /// grant keyed by the domain itself. The kernel never reads this: the *server* resolves it
    /// against config into a [`SessionGrant`], keeping the kernel free of the config stack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Opaque pack payload (workspace root, vault path, contract JSON, …).
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// Is anyone *there*? Not a subtype — an attribute, exactly like `goal` (D7).
///
/// Foreground sessions were started by a human who is watching (a chat, a `/spawn`). Background
/// sessions were started by something that isn't: a cron firing, a webhook, a `delegate`d subagent.
///
/// It does **not** decide whether a session may ask a human — that is
/// [`Capability::AskHuman`](liberado_common::Capability::AskHuman) on the grant (S6), which is
/// enforced. This is only "who was watching", for display and for policy defaults. The two are
/// deliberately independent: a background session *may* hold `AskHuman` (it would await an answer
/// nobody is there to give, until its idle budget kills it), and a foreground one may lack it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// A human started it and is (or was) attending it.
    #[default]
    Foreground,
    /// A cron, a webhook, or a dispatched subagent started it. Nobody is watching.
    Background,
}

impl Visibility {
    pub fn is_background(self) -> bool {
        matches!(self, Self::Background)
    }
}

/// Lifecycle of one goal session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Pending,
    Running,
    /// Was awaiting a human answer when the daemon stopped (E6). Not terminal, and deliberately
    /// distinct from `Running`: a `Running` session is mid-computation and cannot survive a
    /// restart, whereas this one was merely *parked on you*, and the only state it needs in order
    /// to continue is the answer it never got.
    ///
    /// It survives replay with `awaiting_input` intact, so the question you were asked is still
    /// visible rather than silently erased. It is **not yet answerable**: no pack is hosting it,
    /// so `send_input` finds no live channel and `POST /api/goals/{id}/message` fails. Restarting
    /// the pack on an answer is E6-c, and until it lands, `Parked` means "this was waiting for
    /// you, and you can see what it wanted" — not "reply and it resumes". Do not widen that claim
    /// without wiring the resume.
    Parked,
    Succeeded,
    Failed,
    Cancelled,
    BudgetExhausted,
}

impl SessionStatus {
    pub fn is_terminal(self) -> bool {
        self.terminal_kind().is_some()
    }

    /// How this session *ended* — `None` while it is still going.
    ///
    /// The inverse of [`From<TerminalKind>`]. Defined as the one place that knows which statuses are
    /// terminal, so [`is_terminal`](Self::is_terminal) cannot disagree with it: the two used to be
    /// independent lists, and a status added to one but not the other is exactly how `Parked` came to
    /// be told it "had already finished".
    pub fn terminal_kind(self) -> Option<TerminalKind> {
        match self {
            Self::Succeeded => Some(TerminalKind::Succeeded),
            Self::Failed => Some(TerminalKind::Failed),
            Self::Cancelled => Some(TerminalKind::Cancelled),
            Self::BudgetExhausted => Some(TerminalKind::BudgetExhausted),
            Self::Pending | Self::Running | Self::Parked => None,
        }
    }
}

/// How the session ended (surface-friendly).
///
/// **This is deliberately not the same type as [`SessionStatus`]**, even though its variants are
/// exactly that type's terminal subset. `SessionStatus` is a *lifecycle* — it includes `Running` and
/// `Parked`, which are not ways a session can *end*. Keeping them separate is what makes it
/// impossible for a pack to return "still running" as its final outcome, and that guarantee is worth
/// more than the four lines of apparent duplication.
///
/// What was **not** worth keeping was the hand-written match that converted between them (it lived in
/// `hub.rs`, and nothing stopped a fifth variant being added to one type and forgotten in the other).
/// Use [`From`] and [`SessionStatus::terminal_kind`]; the compiler now enforces totality in both
/// directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalKind {
    Succeeded,
    Failed,
    Cancelled,
    BudgetExhausted,
}

impl From<TerminalKind> for SessionStatus {
    fn from(t: TerminalKind) -> Self {
        match t {
            TerminalKind::Succeeded => Self::Succeeded,
            TerminalKind::Failed => Self::Failed,
            TerminalKind::Cancelled => Self::Cancelled,
            TerminalKind::BudgetExhausted => Self::BudgetExhausted,
        }
    }
}

/// Pack outcome after a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalResult {
    pub terminal: TerminalKind,
    pub summary: String,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub diagnostics: serde_json::Value,
}

/// Stored session row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalSessionRecord {
    pub id: String,
    pub goal: GoalSpec,
    /// The authority this session runs under (S6) — resolved once at start from its profile (or the
    /// bare domain) and **never widened** thereafter (Decision 4). Recorded here rather than held
    /// only in memory so it lands in the durable JSONL transcript: what a session was *allowed* to
    /// do is as much a part of the audit trail as what it did.
    #[serde(default)]
    pub grant: SessionGrant,
    /// Whether a human was watching when this started (D7). Carried on the *record*, not just on
    /// the store's header, because this lens is what every non-human trigger — a cron, a webhook, a
    /// `delegate`d subagent — writes through. While it was missing here, the store had no choice but
    /// to stamp every session it received `Foreground`, and so nothing could ever record a
    /// background run: the type existed and nothing could emit it.
    #[serde(default)]
    pub visibility: Visibility,
    pub status: SessionStatus,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<GoalResult>,
    #[serde(default)]
    pub event_count: usize,
    /// True while the pack is blocked awaiting human input (interactive sessions). Derived by the
    /// store from `AwaitingInput` / `HumanInput` events, so list/snapshot views can badge sessions
    /// that need a human without scanning the event log.
    #[serde(default)]
    pub awaiting_input: bool,
}

impl GoalSessionRecord {
    /// A session with **zero authority** — the fail-safe default (Decision 4: no ambient authority).
    /// Callers that mean to grant something use [`with_grant`](Self::with_grant).
    pub fn new(goal: GoalSpec) -> Self {
        Self::with_grant(goal, SessionGrant::default())
    }

    pub fn with_grant(goal: GoalSpec, grant: SessionGrant) -> Self {
        let id = goal
            .id
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| Ulid::new().to_string());
        Self {
            id,
            goal,
            grant,
            visibility: Visibility::Foreground,
            status: SessionStatus::Pending,
            created_at: Utc::now(),
            finished_at: None,
            result: None,
            event_count: 0,
            awaiting_input: false,
        }
    }

    /// A session nobody is watching — a cron firing, a webhook, a `delegate`d subagent.
    pub fn background(goal: GoalSpec, grant: SessionGrant) -> Self {
        Self {
            visibility: Visibility::Background,
            ..Self::with_grant(goal, grant)
        }
    }
}

/// What a goal session is allowed to do, and how its pack is configured (S6).
///
/// Two things the server resolves from `[[session_profiles]]` before the kernel starts a session:
/// the **capability set** (the authority ceiling — narrow-only from here) and the pack's **opaque
/// overrides** (role/model/prompt — the kernel never looks inside; only the pack parses them, the
/// same contract `[tuning.coder]` has).
///
/// Defaults to zero authority and no overrides, so a session started without an explicit grant can
/// do nothing — notably it cannot [`AskHuman`](liberado_common::Capability::AskHuman), so it can
/// never block waiting on a person.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionGrant {
    /// The session's authority ceiling.
    #[serde(default)]
    pub capabilities: CapabilitySet,
    /// The profile this grant came from, for display/audit. `None` = the bare domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Opaque, pack-parsed configuration.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub overrides: serde_json::Value,
}

impl SessionGrant {
    /// Whether this session may interrupt a human for guidance — the check the hub makes before it
    /// wires up an input channel at all.
    pub fn grants_ask_human(&self) -> bool {
        self.capabilities.grants_ask_human()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_round_trips_and_is_omitted_when_absent() {
        let with = GoalSpec {
            id: None,
            description: "build a CLI".into(),
            success_criteria: vec![],
            domain: DomainHint::Coding,
            max_turns: 0,
            max_idle_secs: None,
            origin: Some(SessionOrigin {
                conversation_id: Some("01CONV".into()),
                correlation_id: Some("corr-1".into()),
            }),
            profile: None,
            payload: serde_json::json!({}),
        };
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(json["origin"]["conversation_id"], "01CONV");
        let back: GoalSpec = serde_json::from_value(json).unwrap();
        assert_eq!(
            back.origin.as_ref().unwrap().conversation_id.as_deref(),
            Some("01CONV")
        );

        // Absent origin is skipped in the wire form (backward-compatible with human-started goals).
        let without = GoalSpec {
            origin: None,
            ..with
        };
        let json = serde_json::to_value(&without).unwrap();
        assert!(
            json.get("origin").is_none(),
            "origin should be omitted when None"
        );
        let rec = GoalSessionRecord::new(without);
        assert!(rec.goal.origin.is_none());
    }

    /// The two enums cannot drift apart.
    ///
    /// V1: `TerminalKind` is exactly `SessionStatus`'s terminal subset, and the conversion between
    /// them used to be a hand-written match in `hub.rs`. Adding a fifth way for a session to end
    /// would have compiled fine there and been silently dropped. Now the compiler enforces totality
    /// (both matches are exhaustive), and this pins the *semantics* the compiler cannot: that the
    /// round trip is the identity, and that `is_terminal` cannot disagree with `terminal_kind`.
    ///
    /// That disagreement is not hypothetical. `is_terminal` and the replay coercion were once two
    /// independent lists of "which statuses are final", and `Parked` — added to one, forgotten by
    /// the other — got told it "had already finished", which is the one thing it had not done.
    #[test]
    fn terminal_kind_and_session_status_round_trip() {
        for kind in [
            TerminalKind::Succeeded,
            TerminalKind::Failed,
            TerminalKind::Cancelled,
            TerminalKind::BudgetExhausted,
        ] {
            let status = SessionStatus::from(kind);
            assert!(
                status.is_terminal(),
                "{kind:?} is a way a session ENDS, so its status must be terminal"
            );
            assert_eq!(
                status.terminal_kind(),
                Some(kind),
                "the round trip must be the identity — a session that ended one way must not report                  having ended another"
            );
        }
    }

    #[test]
    fn a_session_still_going_has_not_ended_in_any_way() {
        for status in [
            SessionStatus::Pending,
            SessionStatus::Running,
            SessionStatus::Parked,
        ] {
            assert!(!status.is_terminal(), "{status:?} is not an ending");
            assert_eq!(
                status.terminal_kind(),
                None,
                "{status:?} must not be reportable as a way the session ENDED. Parked is the whole                  reason this matters: it is a session waiting for you, and calling it finished is                  the difference between 'start over' and 'wait'."
            );
        }
    }
}
