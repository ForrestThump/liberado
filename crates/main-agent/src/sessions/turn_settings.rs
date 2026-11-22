//! One turn's resolved settings.
//!
//! The struct lives here so `sessions.rs` can carry the workspace flag without
//! crossing its line ceiling. A profile switch must not land between the reads
//! this type gathers.

use liberado_common::CapabilitySet;

use super::ChatSessions;

/// What one turn runs under, resolved from the session's profile (or the daemon's defaults).
///
/// A struct rather than three lookups so a profile switch cannot land between them — a turn running
/// one profile's tools under another's delegation setting would be a genuinely confusing bug to
/// chase, and the cost of preventing it is one type.
pub(super) struct TurnSettings {
    pub(super) capabilities: CapabilitySet,
    /// Whether the face agent may `delegate`. The profile's setting, else the daemon's.
    /// A read-only grant that omits the setting is off.
    pub(super) delegation: bool,
    /// Extra system-prompt text for this profile, injected per turn.
    pub(super) prompt_append: Option<String>,
    /// The profile's name, for logging. `None` is "no profile named", which is distinct from a
    /// profile that grants nothing — the distinction this plan keeps insisting on.
    pub(super) profile: Option<String>,
    /// Model for this session's turns. `None` = the daemon's configured model.
    ///
    /// Resolved here with everything else for the reason this struct exists: a profile switch must
    /// not land between two reads and give a turn one profile's model under another's tools.
    pub(super) model: Option<String>,
    /// An Automation Editor chat. Its face catalog is the schedule tools only.
    pub(super) automation_editor: bool,
    /// Private file tools for this turn. `false` withholds them.
    pub(super) workspace: bool,
}

/// Settings for a chat that names no profile. File tools stay on.
pub(super) fn bare(sessions: &ChatSessions) -> TurnSettings {
    TurnSettings {
        capabilities: sessions.capabilities.clone(),
        delegation: sessions.delegation_mode,
        prompt_append: None,
        profile: None,
        model: None,
        automation_editor: false,
        workspace: true,
    }
}
