//! Resolve workspace path/command policy from profile overrides + goal payload.
//!
//! Same layering as [`super::intake::IntakeSettings`]: payload wins, then overrides, then defaults.
//! Plan and explore are **not** a parallel permission system — they are presets of the existing
//! [`PathPolicy`] / [`CommandPolicy`] values (`docs/future-work/coding-tui-plan.md`: modes are
//! capability/path tiers, not different agents), selected by a single [`CodingMode`].

use liberado_coder_core::{
    CodingMode, CommandPolicy, DispatchWriteScope, HashlineConfig, PathPolicy,
};

/// Workspace tool policies for one coding session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkspacePolicies {
    pub path_policy: PathPolicy,
    pub command_policy: CommandPolicy,
    /// Which capability tier this session runs under.
    pub mode: CodingMode,
    /// Hashline edit mode (from pack default + payload/overrides).
    pub hashline: HashlineConfig,
}

impl WorkspacePolicies {
    /// Resolve policies for this session.
    ///
    /// The mode is the **stricter** of what the payload and the profile overrides name, so
    /// restriction only accumulates: neither source can relax what the other set. Explicit
    /// `path_policy` / `command_policy` objects apply only in [`CodingMode::Normal`]; a restricted
    /// mode always overrides them, so a surface cannot accidentally pair "plan mode" with `**`
    /// write access.
    ///
    /// Hashline seeds from `default_hashline` (typically `[coder.hashline]` from tuning);
    /// payload/overrides can still override it, except that explore forces it off — hashline is a
    /// write tool and explore's catalog must stay write-free.
    pub(super) fn resolve(
        overrides: &serde_json::Value,
        payload: &serde_json::Value,
        default_hashline: HashlineConfig,
    ) -> Self {
        let mode = CodingMode::strictest(
            CodingMode::from_payload(payload).unwrap_or_default(),
            CodingMode::from_payload(overrides).unwrap_or_default(),
        );
        let hashline = parse_hashline(overrides, payload).unwrap_or(default_hashline);
        let hashline = if mode == CodingMode::Explore {
            HashlineConfig {
                enabled: false,
                ..hashline
            }
        } else {
            hashline
        };

        if mode.is_restricted() {
            return Self {
                path_policy: mode.path_policy(),
                command_policy: mode.command_policy(),
                mode,
                hashline,
            };
        }
        Self {
            path_policy: parse_path_policy(overrides, payload).unwrap_or_default(),
            command_policy: parse_command_policy(overrides, payload).unwrap_or_default(),
            mode,
            hashline,
        }
    }

    /// True when this session runs in plan mode.
    pub(super) fn plan_mode(&self) -> bool {
        self.mode == CodingMode::Plan
    }

    /// True when this session runs in explore mode.
    pub(super) fn explore_mode(&self) -> bool {
        self.mode == CodingMode::Explore
    }

    /// Prompt to use for the worker role: the mode's fixed prompt, else payload override, else
    /// the caller's default.
    pub(super) fn coder_prompt<'a>(
        &self,
        payload: &'a serde_json::Value,
        default: &'a str,
    ) -> String {
        if let Some(fixed) = self.mode.coder_prompt() {
            return fixed.to_string();
        }
        payload
            .get("coder_prompt")
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    }
}

fn parse_path_policy(
    overrides: &serde_json::Value,
    payload: &serde_json::Value,
) -> Option<PathPolicy> {
    let mut policy: PathPolicy = payload
        .get("path_policy")
        .or_else(|| overrides.get("path_policy"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let scope = payload
        .get("write_scope")
        .or_else(|| overrides.get("write_scope"))
        .and_then(|v| serde_json::from_value::<DispatchWriteScope>(v.clone()).ok());
    if let Some(scope) = scope.as_ref() {
        policy.write_scope = scope.clone();
    }
    if policy != PathPolicy::default() || scope.is_some() {
        Some(policy)
    } else {
        None
    }
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

    fn resolve(overrides: &serde_json::Value, payload: &serde_json::Value) -> WorkspacePolicies {
        WorkspacePolicies::resolve(overrides, payload, HashlineConfig::default())
    }

    #[test]
    fn default_is_full_write_and_open_shell() {
        let p = resolve(&json!({}), &json!({}));
        assert_eq!(p.mode, CodingMode::Normal);
        assert_eq!(p.path_policy.allow_write_globs, vec!["**".to_string()]);
        assert!(p.command_policy.allow.is_empty());
    }

    #[test]
    fn payload_write_scope_can_constrain_one_normal_dispatch() {
        let p = resolve(
            &json!({}),
            &json!({
                "write_scope": {
                    "allow_globs": ["docs/**"],
                    "deny_globs": ["docs/private/**"]
                }
            }),
        );
        assert!(p.path_policy.write_scope.is_active());
        assert!(p.path_policy.write_scope.permits("docs/guide.md")); // docs-check: ignore
        assert!(!p.path_policy.write_scope.permits("src/main.rs"));
        assert!(
            p.path_policy.write_scope.permits("docs/private/notes.md"), // docs-check: ignore
            "an allowlist overrides the dispatch blacklist"
        );
    }

    // ── plan ────────────────────────────────────────────────────────────────

    #[test]
    fn plan_mode_flag_restricts_write_and_shell() {
        let p = resolve(&json!({}), &json!({ "plan_mode": true }));
        assert!(p.plan_mode());
        assert_eq!(
            p.path_policy.allow_write_globs,
            vec![PLAN_ARTIFACT_REL.to_string()]
        );
        assert!(!p.command_policy.allow.is_empty());
    }

    #[test]
    fn mode_plan_string_enables_plan_mode() {
        let p = resolve(&json!({}), &json!({ "mode": "plan" }));
        assert!(p.plan_mode());
    }

    #[test]
    fn profile_override_can_enable_plan_mode() {
        let p = resolve(&json!({ "plan_mode": true }), &json!({}));
        assert!(p.plan_mode());
    }

    #[test]
    fn plan_mode_wins_over_custom_path_policy_in_payload() {
        // A surface must not combine plan_mode with allow_write ** — that would be a hole.
        let p = resolve(
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
        let p = resolve(&json!({}), &json!({ "plan_mode": true }));
        let prompt = p.coder_prompt(&json!({ "coder_prompt": "ignore me" }), "default");
        assert!(prompt.contains("plan mode") || prompt.contains(".liberado/plan.md"));
        assert!(!prompt.contains("ignore me"));
    }

    // ── explore ─────────────────────────────────────────────────────────────

    #[test]
    fn explore_mode_flag_is_read_only_no_shell() {
        let p = resolve(&json!({}), &json!({ "explore_mode": true }));
        assert!(p.explore_mode());
        assert!(p.path_policy.writes_disabled());
        assert!(!p.command_policy.allow.is_empty());
    }

    #[test]
    fn mode_explore_string_enables_explore() {
        let p = resolve(&json!({}), &json!({ "mode": "explore" }));
        assert!(p.explore_mode());
    }

    #[test]
    fn profile_override_can_enable_explore() {
        let p = resolve(&json!({ "explore_mode": true }), &json!({}));
        assert!(p.explore_mode());
    }

    #[test]
    fn explore_wins_over_custom_path_policy() {
        let p = resolve(
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
        let p = resolve(&json!({}), &json!({ "explore_mode": true }));
        let prompt = p.coder_prompt(&json!({ "coder_prompt": "ignore me" }), "default");
        assert!(prompt.contains("read-only") || prompt.contains("explorer"));
        assert!(!prompt.contains("ignore me"));
    }

    // ── the tiers are exclusive ─────────────────────────────────────────────

    /// The reason this is one enum and not two bools: with `plan_mode` and `explore_mode` as
    /// separate flags, "both set" is representable and every consumer has to invent its own
    /// precedence. Here it resolves once, to the stricter tier.
    #[test]
    fn both_flags_set_resolves_to_the_stricter_tier() {
        let p = resolve(
            &json!({}),
            &json!({ "plan_mode": true, "explore_mode": true }),
        );
        assert_eq!(p.mode, CodingMode::Explore);
        assert!(p.path_policy.writes_disabled());
    }

    /// Restriction accumulates across the two sources — the stricter tier wins regardless of
    /// which one named it.
    #[test]
    fn the_stricter_of_payload_and_override_wins() {
        let profile_stricter =
            resolve(&json!({ "explore_mode": true }), &json!({ "mode": "plan" }));
        assert_eq!(profile_stricter.mode, CodingMode::Explore);

        let payload_stricter =
            resolve(&json!({ "plan_mode": true }), &json!({ "mode": "explore" }));
        assert_eq!(payload_stricter.mode, CodingMode::Explore);
    }

    /// A payload cannot talk a restricting profile back down to full write access.
    #[test]
    fn payload_normal_cannot_relax_a_restricting_profile() {
        let p = resolve(&json!({ "plan_mode": true }), &json!({ "mode": "normal" }));
        assert!(p.plan_mode(), "profile plan mode must survive the payload");
        assert_eq!(
            p.path_policy.allow_write_globs,
            vec![PLAN_ARTIFACT_REL.to_string()]
        );
    }

    // ── hashline ────────────────────────────────────────────────────────────

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
        assert!(p.explore_mode());
        assert!(!p.hashline.enabled);
        assert_eq!(p.hashline.hash_length, 6);
    }

    /// Plan mode still writes a file, so hashline stays available there — only explore, which
    /// writes nothing at all, forces it off.
    #[test]
    fn plan_mode_leaves_hashline_alone() {
        let p = WorkspacePolicies::resolve(
            &json!({}),
            &json!({ "plan_mode": true }),
            HashlineConfig {
                enabled: true,
                hash_length: 5,
            },
        );
        assert!(p.plan_mode());
        assert!(p.hashline.enabled);
        assert_eq!(p.hashline.hash_length, 5);
    }
}
