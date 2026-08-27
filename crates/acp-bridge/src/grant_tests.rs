//! Split from `coding_run.rs` for module-health boundaries.

use super::*;

/// A configured deployment missing the grant must be refused by name, not run with implied
/// authority. This is the fail-closed half; without it the grant is a row nothing reads.
/// A *loadable* config dir with policy.toml but no `coding-local` grant.
///
/// Writing only policy.toml made `load_config` fail on the missing topology, and that load
/// error string happens to contain "coding-local" too — so the first version of this test
/// passed with the emptiness check deleted. It asserted the error message, not the rule.
fn config_dir_without_the_grant() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("topology.toml"),
        "vault_path = \"/tmp/vault\"\n",
    )
    .expect("write topology");
    std::fs::write(
        dir.path().join("policy.toml"),
        "[[grants]]\ncomponent = \"something-else\"\ncapabilities = [\"AskHuman\"]\n",
    )
    .expect("write policy");
    dir
}

#[tokio::test]
async fn a_configured_deployment_without_the_grant_is_refused() {
    let dir = config_dir_without_the_grant();
    // Precondition: the config must actually LOAD, or this proves nothing about the grant.
    liberado_config::load_config(Some(dir.path()))
        .expect("fixture config must load - otherwise the refusal below is a load error");

    let err = resolve_local_grant(Some(dir.path()))
        .expect_err("a missing grant must refuse, not default to permissive");
    assert!(
        err.contains("resolves to nothing"),
        "must refuse for the empty grant specifically, not some upstream failure: {err}"
    );
}

/// Standalone (no config dir) is the common install and must keep working — the refusal above
/// must not fire when there is no deployment to have a policy at all.
#[tokio::test]
async fn standalone_without_a_config_dir_is_allowed() {
    assert!(
        resolve_local_grant(None).is_ok(),
        "no config dir means no deployment, not a refusal"
    );
}
