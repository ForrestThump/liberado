//! Safety-critical configuration arrival and fail-closed default contract tests (Items 3 & 4).
//!
//! Verifies that absent/optional security and capability boundaries default to fail-closed
//! (refusal) rather than permissive behavior, and that security configuration values
//! loaded from real TOML files arrive at the live execution interfaces without being bypassed.

use super::{POLICY_FILE, TOPOLOGY_FILE, load_config};
use liberado_common::{Capability, WriteClass, Zone};
use liberado_config_loader::{CodingAuthError, CodingWorkspaceAuth, SubagentIsolation};

const TOPOLOGY_TEMPLATE: &str = r#"
vault_path = "{VAULT_PATH}"

[[projects]]
name = "core"
root = "{CORE_ROOT}"

[[projects]]
name = "web"
root = "{WEB_ROOT}"

[[session_profiles]]
name = "safe_reviewer"
domain = "coding"
"#;

const POLICY_BASE: &str = r#"
[[zones]]
zone = "safe_zone"
write_class = "agent_writable"

[[grants]]
component = "main-agent"
capabilities = [{ Read = { Vault = "safe_zone" } }]

[[grants]]
component = "safe_reviewer"
capabilities = [{ Read = { Vault = "safe_zone" } }]
"#;

fn load_test_config() -> liberado_config_loader::Config {
    let _guard = match crate::survivor_tests::env_lock().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let data = tempfile::TempDir::new().unwrap();
    let _env = crate::survivor_tests::EnvGuard::set("LIBERADO_DATA_DIR", data.path());

    let dir = tempfile::TempDir::new().unwrap();
    let core_dir = dir.path().join("core");
    let web_dir = dir.path().join("web");
    let vault_dir = dir.path().join("vault");
    std::fs::create_dir_all(&core_dir).unwrap();
    std::fs::create_dir_all(&web_dir).unwrap();
    std::fs::create_dir_all(&vault_dir).unwrap();

    let topology_toml = TOPOLOGY_TEMPLATE
        .replace(
            "{CORE_ROOT}",
            &core_dir.display().to_string().replace('\\', "/"),
        )
        .replace(
            "{WEB_ROOT}",
            &web_dir.display().to_string().replace('\\', "/"),
        )
        .replace(
            "{VAULT_PATH}",
            &vault_dir.display().to_string().replace('\\', "/"),
        );

    std::fs::write(dir.path().join(TOPOLOGY_FILE), topology_toml).unwrap();
    std::fs::write(dir.path().join(POLICY_FILE), POLICY_BASE).unwrap();

    let (config, _) = load_config(Some(dir.path())).expect("valid fixture must load");
    config
}

#[test]
fn undeclared_zone_defaults_to_proposal_only_refusal() {
    let config = load_test_config();

    // Declared zone has its configured write class
    assert_eq!(
        config.policy.write_class("safe_zone"),
        WriteClass::AgentWritable
    );

    // Any undeclared zone defaults to ProposalOnly (refuses direct agent writes)
    assert_eq!(
        config.policy.write_class("unregistered_zone"),
        WriteClass::ProposalOnly,
        "undeclared zone must fail-closed to ProposalOnly"
    );
    assert_eq!(
        config.policy.write_class("System/Secrets"),
        WriteClass::ProposalOnly,
        "arbitrary path must fail-closed to ProposalOnly"
    );
}

#[test]
fn unlisted_component_receives_empty_capability_grant() {
    let config = load_test_config();

    // Declared component gets its exact granted capabilities
    let main_caps = config.policy.capabilities_for("main-agent");
    assert!(
        main_caps
            .capabilities
            .contains(&Capability::Read(Zone::Vault("safe_zone".into())))
    );

    // Any unlisted component receives an empty capability set (fail-closed refusal)
    let unknown_caps = config.policy.capabilities_for("unauthorized_subagent");
    assert!(
        unknown_caps.capabilities.is_empty(),
        "unlisted component must receive empty capability set"
    );
}

#[test]
fn unknown_coding_project_name_fails_closed() {
    let config = load_test_config();

    // Resolving an unknown project name must be rejected with UnknownProject
    let result = config.authorize_coding_workspace(Some("nonexistent_project_id"), None);
    match result {
        Err(CodingAuthError::UnknownProject { name }) => {
            assert_eq!(name, "nonexistent_project_id");
        }
        other => panic!("expected UnknownProject error, got: {other:?}"),
    }
}

#[test]
fn unconfigured_workspace_defaults_to_ephemeral_sandbox() {
    let config = load_test_config();

    // No name and no path defaults safely to Ephemeral (isolated temporary sandbox)
    let auth = config
        .authorize_coding_workspace(None, None)
        .expect("unspecified workspace must resolve to ephemeral");
    assert_eq!(
        auth,
        CodingWorkspaceAuth::Ephemeral,
        "unspecified workspace must default to Ephemeral sandbox"
    );
}

#[test]
fn undeclared_session_profile_fails_closed() {
    let config = load_test_config();

    // Requesting an unknown profile name returns Err rather than an unconstrained session
    assert!(
        config
            .resolve_session_profile(Some("nonexistent_profile"), "coding")
            .is_err(),
        "unknown profile must return Err"
    );

    // Configured profile resolves to its exact declared domain
    let profile = config
        .resolve_session_profile(Some("safe_reviewer"), "coding")
        .expect("declared profile must resolve");
    assert_eq!(profile.domain.as_deref(), Some("coding"));
}

#[test]
fn subagent_isolation_defaults_safely() {
    let config = load_test_config();

    // Subagent isolation configuration is initialized and bounded
    assert_eq!(
        config.tuning.dispatch.subagent_isolation,
        SubagentIsolation::InProcess
    );
}
