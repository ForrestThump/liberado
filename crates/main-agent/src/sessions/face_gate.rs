//! Whether a turn uses the face runtime.
//!
//! The Automation Editor flag selects that runtime even when the profile turns
//! delegation off. `delegate` is still absent from the editor catalog.

use super::{ChatSessions, TurnSettings};

impl ChatSessions {
    /// Face path for this turn. The editor flag wins over a delegation-off profile.
    pub(super) fn face_turn(&self, settings: &TurnSettings) -> bool {
        self.face_turn_with(settings.delegation, settings.automation_editor)
    }

    fn face_turn_with(&self, delegation: bool, editor: bool) -> bool {
        editor || (delegation && self.face_bridge.is_some())
    }
}
