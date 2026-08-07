//! Resolve workspace path/command policy from profile overrides + goal payload.
//!
//! Same layering as [`super::intake::IntakeSettings`]: payload wins, then overrides, then defaults.
//! Plan and explore modes are **not** parallel permission systems — they are presets of existing
//! [`PathPolicy`] / [`CommandPolicy`] values (capability/path tiers, not different agents).
//!
//! If both modes are requested, **explore wins** (strictest: fully read-only). Plan allows writing
//! only `.liberado/plan.md`; explore allows no writes.

use liberado_coder_core::{
    CommandPolicy, EXPLORE_MODE_CODER_PROMPT, HashlineConfig, PLAN_MODE_CODER_PROMPT, PathPolicy,
};

/// Workspace tool policies for one coding session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkspacePolicies {
    pub path_policy: PathPolicy,
    pub command_policy: CommandPolicy,
    /// When true, the session is plan mode (exclusive plan-file writes, no shell).
    pub plan_mode: bool,
    /// Read-only explore subagent: no writes, no shell, read-only tool catalog.
    pub explore_mode: bool,
    /// Hashline edit mode (from pack default + payload/overrides).
    pub hashline: HashlineConfig,
}

impl WorkspacePolicies {
    /// Resolve policies for this session.
    ///
    /// Plan mode activates when any of:
    /// - `payload.plan_mode` / `overrides.plan_mode` is true
    /// - `payload.mode` / `overrides.mode` is the string `"plan"`
    ///
    /// Explore mode activates when any of:
    /// - `payload.explore_mode` / `overrides.explore_mode` is true
    /// - `payload.mode` / `overrides.mode` is the string `"explore"`
    ///
    /// Seeds hashline from `default_hashline` (typically `[coder.hashline]` from tuning);
    /// payload/overrides can still override. If both plan and explore are requested, explore wins
    /// (strictest). Explicit `path_policy` / `command_policy` objects apply only when neither mode
    /// is active.
    pub(super) fn resolve(
        overrides: &serde_json::Value,
        payload: &serde_json::Value,
        default_hashline: HashlineConfig,
    ) -> Self {
        let explore_mode = is_explore_mode(overrides, payload);
        // Explore is stricter than plan; do not leave plan_mode true alongside explore.
        let plan_mode = !explore_mode && is_plan_mode(overrides, payload);
        let hashline = parse_hashline(overrides, payload).unwrap_or(default_hashline);
        if explore_mode {
            return Self {
                path_policy: PathPolicy::read_only(),
                command_policy: CommandPolicy::none_allowed(),
                plan_mode: false,
                explore_mode: true,
                // Explore is read-only — keep hashline off so the catalog stays write-free.
                hashline: HashlineConfig {
                    enabled: false,
                    ..hashline
                },
            };
        }
        if plan_mode {
            return Self {
                path_policy: PathPolicy::plan_mode(),
                command_policy: CommandPolicy::none_allowed(),
                plan_mode: true,
                explore_mode: false,
                hashline,
            };
        }
        Self {
            path_policy: parse_path_policy(overrides, payload).unwrap_or_default(),
            command_policy: parse_command_policy(overrides, payload).unwrap_or_default(),
            plan_mode: false,
            explore_mode: false,
            hashline,
        }
    }

    /// Prompt for the worker role: explore/plan fixed prompts, else payload override, else default.
    pub(super) fn coder_prompt<'a>(
        &self,
        payload: &'a serde_json::Value,
        default: &'a str,
    ) -> String {
        if self.explore_mode {
            return EXPLORE_MODE_CODER_PROMPT.to_string();
        }
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

fn parse_hashline(
    overrides: &serde_json::Value,
    payload: &serde_json::Value,
) -> Option<HashlineConfig> {
    payload
        .get("hashline")
        .or_else(|| overrides.get("hashline"))
        .and_then(|v| serde_json::from_value::<HashlineConfig>(v.clone()).ok())
        .and_then(|cfg| cfg.validate().ok().map(|_| cfg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_coder_core::PLAN_ARTIFACT_REL;
    use serde_json::json;

    #[test]
    fn default_is_full_write_and_open_shell() {
        let p = WorkspacePolicies::resolve(&json!({}), &json!({}), HashlineConfig::default());
        assert!(!p.plan_mode);
        assert!(!p.explore_mode);
        assert_eq!(p.path_policy.allow_write_globs, vec!["**".to_string()]);
        assert!(p.command_policy.allow.is_empty());
    }

    #[test]
    fn plan_mode_flag_restricts_write_and_shell() {
        let p = WorkspacePolicies::resolve(&json!({}), &json!({ "plan_mode": true }), HashlineConfig::default());
        assert!(p.plan_mode);
        assert!(!p.explore_mode);
        assert_eq!(
            p.path_policy.allow_write_globs,
            vec![PLAN_ARTIFACT_REL.to_string()]
        );
        assert!(!p.command_policy.allow.is_empty());
    }

    #[test]
    fn explore_mode_flag_is_read_only_no_shell() {
        let p = WorkspacePolicies::resolve(&json!({}), &json!({ "explore_mode": true }), HashlineConfig::default());
        assert!(p.explore_mode);
        assert!(!p.plan_mode);
        assert!(p.path_policy.writes_disabled());
        assert!(!p.command_policy.allow.is_empty());
    }

    #[test]
    fn mode_plan_string_enables_plan_mode() {
        let p = WorkspacePolicies::resolve(&json!({}), &json!({ "mode": "plan" }), HashlineConfig::default());
        assert!(p.plan_mode);
    }

    #[test]
    fn mode_explore_string_enables_explore() {
        let p = WorkspacePolicies::resolve(&json!({}), &json!({ "mode": "explore" }), HashlineConfig::default());
        assert!(p.explore_mode);
    }

    #[test]
    fn profile_override_can_enable_plan_mode() {
        let p = WorkspacePolicies::resolve(&json!({ "plan_mode": true }), &json!({}), HashlineConfig::default());
        assert!(p.plan_mode);
    }

    #[test]
    fn profile_override_can_enable_explore() {
        let p = WorkspacePolicies::resolve(&json!({ "explore_mode": true }), &json!({}), HashlineConfig::default());
        assert!(p.explore_mode);
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
            HashlineConfig::default(),
        );
        assert_eq!(
            p.path_policy.allow_write_globs,
            vec![PLAN_ARTIFACT_REL.to_string()]
        );
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
            HashlineConfig::default(),
        );
        assert!(p.path_policy.writes_disabled());
    }

    #[test]
    fn explore_wins_when_both_modes_requested() {
        let p = WorkspacePolicies::resolve(
            &json!({}),
            &json!({ "plan_mode": true, "explore_mode": true }),
            HashlineConfig::default(),
        );
        assert!(p.explore_mode);
        assert!(!p.plan_mode);
        assert!(p.path_policy.writes_disabled());
    }

    #[test]
    fn coder_prompt_in_plan_mode_is_fixed() {
        let p = WorkspacePolicies::resolve(&json!({}), &json!({ "plan_mode": true }), HashlineConfig::default());
        let prompt = p.coder_prompt(&json!({ "coder_prompt": "ignore me" }), "default");
        assert!(prompt.contains("plan mode") || prompt.contains(".liberado/plan.md"));
        assert!(!prompt.contains("ignore me"));
    }

    #[test]
    fn coder_prompt_in_explore_mode_is_fixed() {
        let p = WorkspacePolicies::resolve(&json!({}), &json!({ "explore_mode": true }), HashlineConfig::default());
        let prompt = p.coder_prompt(&json!({ "coder_prompt": "ignore me" }), "default");
        assert!(prompt.contains("read-only") || prompt.contains("explorer"));
        assert!(!prompt.contains("ignore me"));
    }

    #[test]
    fn coder_prompt_normal_mode_uses_payload_override() {
        let p = WorkspacePolicies::resolve(&json!({}), &json!({ "coder_prompt": "be concise" }), HashlineConfig::default());
        let prompt = p.coder_prompt(&json!({ "coder_prompt": "be concise" }), "default");
        assert_eq!(prompt, "be concise");
    }

    #[test]
    fn coder_prompt_normal_mode_falls_back_to_default() {
        let p = WorkspacePolicies::resolve(&json!({}), &json!({}), HashlineConfig::default());
        let prompt = p.coder_prompt(&json!({}), "default prompt");
        assert_eq!(prompt, "default prompt");
    }

    #[test]
    fn parse_path_policy_from_payload() {
        let overrides = json!({});
        let payload = json!({
            "path_policy": {
                "allow_write_globs": ["src/**"],
                "deny_globs": ["secret/**"],
                "read_max_bytes": 4096,
                "search_max_results": 50
            }
        });
        let p = WorkspacePolicies::resolve(&overrides, &payload, HashlineConfig::default());
        assert_eq!(p.path_policy.allow_write_globs, vec!["src/**"]);
        assert_eq!(p.path_policy.deny_globs, vec!["secret/**"]);
        assert_eq!(p.path_policy.read_max_bytes, 4096);
        assert_eq!(p.path_policy.search_max_results, 50);
        assert!(!p.plan_mode);
        assert!(!p.explore_mode);
    }

    #[test]
    fn parse_command_policy_from_overrides() {
        let overrides = json!({
            "command_policy": {
                "allow": ["cargo"],
                "deny": ["rm"],
                "timeout_secs": 120,
                "output_max_bytes": 65536
            }
        });
        let payload = json!({});
        let p = WorkspacePolicies::resolve(&overrides, &payload, HashlineConfig::default());
        assert_eq!(p.command_policy.allow, vec!["cargo"]);
        assert_eq!(p.command_policy.deny, vec!["rm"]);
    }

    #[test]
    fn payload_policy_wins_over_override_policy() {
        let overrides = json!({
            "path_policy": { "allow_write_globs": ["overrides/**"], "read_max_bytes": 4096, "search_max_results": 50 }
        });
        let payload = json!({
            "path_policy": { "allow_write_globs": ["payload/**"], "read_max_bytes": 8192, "search_max_results": 100 }
        });
        let p = WorkspacePolicies::resolve(&overrides, &payload, HashlineConfig::default());
        assert_eq!(p.path_policy.allow_write_globs, vec!["payload/**"]);
    }

    #[test]
    fn hashline_from_payload_overrides_default() {
        let p = WorkspacePolicies::resolve(
            &json!({}),
            &json!({ "hashline": { "enabled": true, "hash_length": 8 } }),
            HashlineConfig {
                enabled: false,
                hash_length: 4,
            },
        );
        assert!(p.hashline.enabled);
        assert_eq!(p.hashline.hash_length, 8);
    }

    #[test]
    fn explore_mode_forces_hashline_off() {
        let p = WorkspacePolicies::resolve(
            &json!({}),
            &json!({
                "explore_mode": true,
                "hashline": { "enabled": true, "hash_length": 6 }
            }),
            HashlineConfig::default(),
        );
        assert!(p.explore_mode);
        assert!(!p.hashline.enabled);
        assert_eq!(p.hashline.hash_length, 6);
    }
}
