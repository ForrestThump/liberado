//! Split from `session_pack/policies.rs`: kills the baseline campaign's
//! survivors in path/command policy parsing.

use super::*;

#[test]
fn a_distinct_path_policy_survives_parsing() {
    let payload = serde_json::json!({ "path_policy": { "read_max_bytes": 999 } });
    let resolved =
        WorkspacePolicies::resolve(&serde_json::json!({}), &payload, HashlineConfig::default());
    assert_eq!(
        resolved.path_policy.read_max_bytes, 999,
        "a non-default payload policy must be honoured"
    );
}

#[test]
fn a_write_scope_alone_is_enough_to_apply_the_policy() {
    let payload = serde_json::json!({ "write_scope": { "deny_globs": ["secrets/**"] } });
    let resolved =
        WorkspacePolicies::resolve(&serde_json::json!({}), &payload, HashlineConfig::default());
    assert_eq!(
        resolved.path_policy.write_scope.deny_globs,
        vec!["secrets/**".to_string()],
        "a scope with default-equal policy must still apply"
    );
}

#[test]
fn a_payload_command_policy_is_honoured() {
    let payload = serde_json::json!({ "command_policy": { "timeout_secs": 77, "output_max_bytes": 4096, "allow": ["cargo"] } });
    let resolved =
        WorkspacePolicies::resolve(&serde_json::json!({}), &payload, HashlineConfig::default());
    assert_eq!(
        resolved.command_policy.timeout_secs, 77,
        "the payload's timeout must not fall back to the default"
    );
}

#[test]
fn an_empty_payload_yields_defaults() {
    let resolved = WorkspacePolicies::resolve(
        &serde_json::json!({}),
        &serde_json::json!({}),
        HashlineConfig::default(),
    );
    assert_eq!(resolved.mode, CodingMode::Normal);
}
