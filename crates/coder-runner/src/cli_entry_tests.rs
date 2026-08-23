//! CLI entry seams: `read_request`, `provider_profile`, `run_request` and the early-refusal
//! path of `run_headless`.
//!
//! These live outside `main.rs` for the same module-health reason as `task_context_tests` —
//! and because dropping them from `main.rs`'s old test mods silently lost their coverage,
//! which the CRAP ratchet caught as a regression on `run_headless`.

use super::{
    HeadlessArgs, Topology, provider_profile, provider_profile_named, read_request, run_headless,
    run_request,
};
use tempfile::TempDir;

#[tokio::test]
async fn read_request_names_a_missing_file_and_rejects_garbage() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("nope.json");
    let err = read_request(Some(&missing))
        .await
        .expect_err("a missing request file must be an error");
    assert!(err.contains("read request"), "{err}");

    let garbage = dir.path().join("request.json");
    std::fs::write(&garbage, b"{ not json }").unwrap();
    let err = read_request(Some(&garbage))
        .await
        .expect_err("unparseable JSON must be an error");
    assert!(err.contains("parse CoderRunRequest"), "{err}");
}

#[test]
fn provider_profile_resolves_the_declared_topology_entry() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("topology.toml"),
        r#"
vault_path = "/tmp/vault"
provider = "testprov"

[[providers]]
name = "testprov"
base_url = "http://127.0.0.1:9"
default_model = "m1"
api_key_env = "TEST_PROV_KEY"
"#,
    )
    .unwrap();

    let profile = provider_profile(Some(dir.path())).expect("declared provider resolves");
    assert_eq!(profile.name, "testprov");
    assert_eq!(profile.api_key_env, "TEST_PROV_KEY");
    assert_eq!(profile.base_url, "http://127.0.0.1:9");
}

#[test]
fn provider_profile_named_reports_an_undeclared_provider() {
    let topology = Topology::default();
    let declared = topology.provider.clone();
    let err = provider_profile_named(topology.clone(), "no-such-provider")
        .expect_err("an undeclared name must fail naming itself");
    assert!(err.contains("no-such-provider"), "{err}");
    assert!(
        provider_profile_named(topology, &declared).is_ok(),
        "the default topology's own provider still resolves"
    );
}

#[tokio::test]
async fn run_request_surfaces_read_failures_before_any_backend_work() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("req.json");
    let err = run_request(Some(missing), None)
        .await
        .expect_err("a missing request file fails fast");
    assert!(err.contains("read request"), "{err}");
}

#[tokio::test]
async fn run_headless_demands_the_api_key_env_before_touching_the_workspace() {
    let args = HeadlessArgs {
        prompt: "do a thing".into(),
        workspace: std::env::temp_dir(),
        model: None,
        max_turns: None,
        config_dir: None,
        api_key_env: Some("LIBERADO_TEST_NO_SUCH_KEY_ENV".into()),
        base_url: None,
        session_id: None,
    };
    let err = run_headless(args)
        .await
        .expect_err("a missing key env refuses before any run happens");
    assert!(err.contains("LIBERADO_TEST_NO_SUCH_KEY_ENV"), "{err}");
    assert!(err.contains("required"), "{err}");
}
