//! Resolve workspace path/command policy from profile overrides + goal payload.
//!
//! Same layering as [`super::intake::IntakeSettings`]: payload wins, then overrides, then defaults.
//! Explore mode is **not** a parallel permission system — it is a preset of existing
//! [`PathPolicy`] / [`CommandPolicy`] values (read-only path policy + no shell), matching the
//! coding-tui plan: modes are capability/path tiers, not different agents.

use liberado_coder_core::{CommandPolicy, EXPLORE_MODE_CODER_PROMPT, PathPolicy};

/// Workspace tool policies for one coding session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkspacePolicies {
    pub path_policy: PathPolicy,
    pub command_policy: CommandPolicy,
    /// Read-only explore subagent: no writes, no shell, read-only tool catalog.
    pub explore_mode: bool,
}

impl WorkspacePolicies {
    /// Resolve policies for this session.
    ///
    /// Explore mode activates when any of:
    /// - `payload.explore_mode` / `overrides.explore_mode` is true
    /// - `payload.mode` / `overrides.mode` is the string `"explore"`
    ///
    /// Explore always wins over custom path/command policies so a surface cannot accidentally
    /// combine explore with full write.
    pub(super) fn resolve(overrides: &serde_json::Value, payload: &serde_json::Value) -> Self {
        let explore_mode = is_explore_mode(overrides, payload);
        if explore_mode {
            return Self {
                path_policy: PathPolicy::read_only(),
                command_policy: CommandPolicy::none_allowed(),
                explore_mode: true,
            };
        }
        Self {
            path_policy: parse_path_policy(overrides, payload).unwrap_or_default(),
            command_policy: parse_command_policy(overrides, payload).unwrap_or_default(),
            explore_mode: false,
        }
    }

    /// Prompt for the worker role: explore fixed prompt, else payload override, else default.
    pub(super) fn coder_prompt<'a>(
        &self,
        payload: &'a serde_json::Value,
        default: &'a str,
    ) -> String {
        if self.explore_mode {
            return EXPLORE_MODE_CODER_PROMPT.to_string();
        }
        payload
            .get("coder_prompt")
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    }
}

fn is_explore_mode(overrides: &serde_json::Value, payload: &serde_json::Value) -> bool {
    let bool_flag = |root: &serde_json::Value| {
        root.get("explore_mode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };
    let mode_explore = |root: &serde_json::Value| {
        root.get("mode")
            .and_then(|v| v.as_str())
            .is_some_and(|m| m.eq_ignore_ascii_case("explore"))
    };
    bool_flag(payload) || mode_explore(payload) || bool_flag(overrides) || mode_explore(overrides)
}

fn parse_path_policy(
    overrides: &serde_json::Value,
    payload: &serde_json::Value,
) -> Option<PathPolicy> {
    payload
        .get("path_policy")
        .or_else(|| overrides.get("path_policy"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

fn parse_command_policy(
    overrides: &serde_json::Value,
    payload: &serde_json::Value,
) -> Option<CommandPolicy> {
    payload
        .get("command_policy")
        .or_else(|| overrides.get("command_policy"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_is_full_write_and_open_shell() {
        let p = WorkspacePolicies::resolve(&json!({}), &json!({}));
        assert!(!p.explore_mode);
        assert_eq!(p.path_policy.allow_write_globs, vec!["**".to_string()]);
        assert!(p.command_policy.allow.is_empty());
    }

    #[test]
    fn explore_mode_flag_is_read_only_no_shell() {
        let p = WorkspacePolicies::resolve(&json!({}), &json!({ "explore_mode": true }));
        assert!(p.explore_mode);
        assert!(p.path_policy.writes_disabled());
        assert!(!p.command_policy.allow.is_empty());
    }

    #[test]
    fn mode_explore_string_enables_explore() {
        let p = WorkspacePolicies::resolve(&json!({}), &json!({ "mode": "explore" }));
        assert!(p.explore_mode);
    }

    #[test]
    fn profile_override_can_enable_explore() {
        let p = WorkspacePolicies::resolve(&json!({ "explore_mode": true }), &json!({}));
        assert!(p.explore_mode);
    }

    #[test]
    fn explore_wins_over_custom_path_policy() {
        let p = WorkspacePolicies::resolve(
            &json!({}),
            &json!({
                "explore_mode": true,
                "path_policy": {
                    "allow_write_globs": ["**"],
                    "deny_globs": [],
                    "read_max_bytes": 1,
                    "search_max_results": 1
                }
            }),
        );
        assert!(p.path_policy.writes_disabled());
    }

    #[test]
    fn coder_prompt_in_explore_mode_is_fixed() {
        let p = WorkspacePolicies::resolve(&json!({}), &json!({ "explore_mode": true }));
        let prompt = p.coder_prompt(&json!({ "coder_prompt": "ignore me" }), "default");
        assert!(prompt.contains("read-only") || prompt.contains("explorer"));
        assert!(!prompt.contains("ignore me"));
    }
}
