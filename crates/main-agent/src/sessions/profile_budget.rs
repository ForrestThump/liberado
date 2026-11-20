//! Per-profile face turn ceiling.
//!
//! The number lives on the profile, like the provider id. A session keeps the profile name.
//! Each turn looks the name up, so a daemon restart picks up an edited `max_turns` without
//! recreating the session. An absent name keeps the executor budget, which is
//! `[main_agent] max_turns` or the compiled default.

use std::collections::HashMap;

use super::ChatSessions;

impl ChatSessions {
    /// Install profile → turn ceiling. Names omitted from the map keep the executor budget.
    #[must_use]
    pub fn with_profile_max_turns(mut self, budgets: HashMap<String, u32>) -> Self {
        self.profile_max_turns = std::sync::Mutex::new(budgets);
        self
    }

    /// Keep a profile's ceiling when the profile is renamed. A missing entry stays missing.
    pub fn rename_profile_max_turns(&self, from: &str, to: &str) {
        if from == to {
            return;
        }
        let mut budgets = self
            .profile_max_turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(turns) = budgets.remove(from) {
            budgets.insert(to.to_string(), turns);
        }
    }

    /// Drop a profile's ceiling. Later turns on that name use the executor budget.
    pub fn clear_profile_max_turns(&self, profile: &str) {
        self.profile_max_turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(profile);
    }
}
