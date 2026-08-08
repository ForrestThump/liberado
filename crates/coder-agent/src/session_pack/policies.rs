//! Resolve workspace path/command policy from profile overrides + goal payload.
//!
//! Same layering as [`super::intake::IntakeSettings`]: payload wins, then overrides, then defaults.
//! Plan mode is **not** a parallel permission system — it is a preset of existing
//! [`PathPolicy`] / [`CommandPolicy`] values (`docs/future-work/coding-tui-plan.md`: plan mode =
//! capability/path tier, not a different agent).

use liberado_coder_core::{CommandPolicy, PLAN_MODE_CODER_PROMPT, PathPolicy};

/// Workspace tool policies for one coding session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkspacePolicies {
    pub path_policy: PathPolicy,
    pub command_policy: CommandPolicy,
    /// When true, the session is plan mode (exclusive plan-file writes, no shell).
    pub plan_mode: bool,
}

impl WorkspacePolicies {
    /// Resolve policies for this session.
    ///
    /// Plan mode activates when any of:
    /// - `payload.plan_mode` / `overrides.plan_mode` is true
    /// - `payload.mode` / `overrides.mode` is the string `"plan"`
    ///
    /// Explicit `path_policy` / `command_policy` objects in payload or overrides (when present and
    /// plan mode is off) are deserialized if valid; otherwise defaults apply. Plan mode always
    /// wins over custom policies so a surface cannot accidentally "plan mode" with full write.
    pub(super) fn resolve(overrides: &serde_json::Value, payload: &serde_json::Value) -> Self {
        let plan_mode = is_plan_mode(overrides, payload);
        if plan_mode {
            return Self {
                path_policy: PathPolicy::plan_mode(),
                command_policy: CommandPolicy::none_allowed(),
                plan_mode: true,
            };
        }
        Self {
            path_policy: parse_path_policy(overrides, payload).unwrap_or_default(),
            command_policy: parse_command_policy(overrides, payload).unwrap_or_default(),
            plan_mode: false,
        }
    }

    /// Prompt to use for the worker role: plan-mode fixed prompt, else payload override, else default.
    pub(super) fn coder_prompt<'a>(
        &self,
        payload: &'a serde_json::Value,
        default: &'a str,
    ) -> String {
        if self.plan_mode {
            return PLAN_MODE_CODER_PROMPT.to_string();
        }
        payload
            .get("coder_prompt")
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    }
}

fn is_plan_mode(overrides: &serde_json::Value, payload: &serde_json::Value) -> bool {
    let bool_flag = |root: &serde_json::Value| {
        root.get("plan_mode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };
    let mode_plan = |root: &serde_json::Value| {
        root.get("mode")
            .and_then(|v| v.as_str())
            .is_some_and(|m| m.eq_ignore_ascii_case("plan"))
    };
    // Either source may enable plan mode; neither can disable it once the other has set it.
    // The flag ORs rather than layers — a profile override that sets plan_mode cannot be
    // overridden by a payload that omits it, and vice versa.
    bool_flag(payload) || mode_plan(payload) || bool_flag(overrides) || mode_plan(overrides)
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
    use liberado_coder_core::PLAN_ARTIFACT_REL;
    use serde_json::json;

    #[test]
    fn default_is_full_write_and_open_shell() {
        let p = WorkspacePolicies::resolve(&json!({}), &json!({}));
        assert!(!p.plan_mode);
        assert_eq!(p.path_policy.allow_write_globs, vec!["**".to_string()]);
        assert!(p.command_policy.allow.is_empty());
    }

    #[test]
    fn plan_mode_flag_restricts_write_and_shell() {
        let p = WorkspacePolicies::resolve(&json!({}), &json!({ "plan_mode": true }));
        assert!(p.plan_mode);
        assert_eq!(
            p.path_policy.allow_write_globs,
            vec![PLAN_ARTIFACT_REL.to_string()]
        );
        assert!(!p.command_policy.allow.is_empty());
    }

    #[test]
    fn mode_plan_string_enables_plan_mode() {
        let p = WorkspacePolicies::resolve(&json!({}), &json!({ "mode": "plan" }));
        assert!(p.plan_mode);
    }

    #[test]
    fn profile_override_can_enable_plan_mode() {
        let p = WorkspacePolicies::resolve(&json!({ "plan_mode": true }), &json!({}));
        assert!(p.plan_mode);
    }

    #[test]
    fn plan_mode_wins_over_custom_path_policy_in_payload() {
        // A surface must not combine plan_mode with allow_write ** — that would be a hole.
        let p = WorkspacePolicies::resolve(
            &json!({}),
            &json!({
                "plan_mode": true,
                "path_policy": { "allow_write_globs": ["**"], "deny_globs": [], "read_max_bytes": 1, "search_max_results": 1 }
            }),
        );
        assert_eq!(
            p.path_policy.allow_write_globs,
            vec![PLAN_ARTIFACT_REL.to_string()]
        );
    }

    #[test]
    fn coder_prompt_in_plan_mode_is_fixed() {
        let p = WorkspacePolicies::resolve(&json!({}), &json!({ "plan_mode": true }));
        let prompt = p.coder_prompt(&json!({ "coder_prompt": "ignore me" }), "default");
        assert!(prompt.contains("plan mode") || prompt.contains(".liberado/plan.md"));
        assert!(!prompt.contains("ignore me"));
    }
}
