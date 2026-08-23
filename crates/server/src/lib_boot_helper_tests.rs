//! Split from `lib.rs` for module-health boundaries.

use super::*;
use std::sync::Arc;

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn webui_dist_env_overrides_the_builtin_directory() {
    let _g = ENV_LOCK.lock().await;
    // SAFETY(test): serialized above; restored before the guard drops.
    unsafe {
        std::env::set_var("LIBERADO_WEBUI_DIST", "/srv/custom-dist");
    }
    assert_eq!(dist_dir(), "/srv/custom-dist");
    unsafe {
        std::env::remove_var("LIBERADO_WEBUI_DIST");
    }
    assert_eq!(dist_dir(), DIST_DIR);
}

#[tokio::test]
async fn port_resolves_from_env_then_default() {
    let _g = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("LIBERADO_PORT", "5599");
    }
    assert_eq!(resolve_port(), 5599);
    unsafe {
        std::env::set_var("LIBERADO_PORT", "not-a-port");
    }
    assert_eq!(resolve_port(), DEFAULT_PORT, "garbage parses as unset");
    unsafe {
        std::env::remove_var("LIBERADO_PORT");
    }
    assert_eq!(resolve_port(), DEFAULT_PORT);
}

fn config_with_vault(path: &str) -> liberado_bootstrap::Config {
    let mut config = liberado_bootstrap::Config::default();
    config.topology.vault_path = std::path::PathBuf::from(path);
    config
}

#[test]
fn vault_path_cli_argument_wins_then_config_then_error() {
    let with_config = config_with_vault("/vault/from/config");
    assert_eq!(
        resolve_vault_path("/vault/from/cli".into(), &with_config).unwrap(),
        "/vault/from/cli",
        "the CLI argument wins"
    );
    assert_eq!(
        resolve_vault_path(String::new(), &with_config).unwrap(),
        "/vault/from/config",
        "an empty argument falls back to topology"
    );
    let empty = liberado_bootstrap::Config::default();
    assert!(
        resolve_vault_path("  ".into(), &empty).is_err(),
        "both empty is a hard error, not a silent default"
    );
}

fn tool(name: &str) -> liberado_provider::ToolDef {
    liberado_provider::ToolDef {
        name: name.into(),
        description: String::new(),
        parameters: serde_json::json!({}),
    }
}

struct FixedRuntime(Vec<liberado_provider::ToolDef>);
#[async_trait::async_trait]
impl ToolRuntime for FixedRuntime {
    fn catalog(&self) -> Vec<liberado_provider::ToolDef> {
        self.0.clone()
    }
    async fn invoke(&self, _: &liberado_provider::ToolInvocation) -> Result<String, String> {
        Err("unused".into())
    }
}

fn runtime_with(names: &[&str]) -> Arc<dyn ToolRuntime> {
    Arc::new(FixedRuntime(names.iter().map(|n| tool(n)).collect()))
}

fn caps_granting(mcps: &[&str]) -> CapabilitySet {
    let mut caps = CapabilitySet::empty();
    for m in mcps {
        caps.grant(liberado_common::Capability::ExecuteMcp((*m).into()));
    }
    caps
}

/// Normal mode exposes the whole live registry; delegation mode collapses to `delegate`
/// plus only MCP-granted tools — and never an ungranted one.
#[tokio::test]
async fn face_tool_surface_follows_delegation_and_grants() {
    let runtime = runtime_with(&["delegate", "memory:search", "vault:write", "plain_tool"]);

    // Full surface outside delegation.
    let (names, count) = face_tool_surface(&runtime, false, &CapabilitySet::empty());
    assert_eq!(count, 4);
    assert!(names.contains(&"memory:search".to_string()));
    assert!(names.contains(&"plain_tool".to_string()));

    // Delegation without grants: delegate only.
    let (names, count) = face_tool_surface(&runtime, true, &CapabilitySet::empty());
    assert_eq!(names, vec!["delegate".to_string()], "{names:?}");
    assert_eq!(count, 1);

    // Delegation with a memory grant: delegate + memory tools; vault stays out.
    let (names, count) = face_tool_surface(&runtime, true, &caps_granting(&["memory"]));
    assert_eq!(count, 2, "{names:?}");
    assert!(names.contains(&"delegate".to_string()));
    assert!(names.contains(&"memory:search".to_string()));
    assert!(!names.contains(&"vault:write".to_string()));
}

/// The store opens under the resolved sessions root: a stub returning an empty path would
/// strand every conversation in the current directory.
#[tokio::test]
async fn open_session_store_opens_under_the_resolved_root() {
    let dir = tempfile::tempdir().unwrap();
    let _g = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("LIBERADO_DATA_DIR", dir.path());
    }
    let expected_root = liberado_bootstrap::sessions_dir();
    let (root, store) = open_session_store().await;
    unsafe {
        std::env::remove_var("LIBERADO_DATA_DIR");
    }
    drop(_g);
    assert_eq!(
        root, expected_root,
        "the store lives where the rest of the daemon looks for it"
    );
    let headers = store.list_sessions().await;
    assert!(
        headers.is_empty(),
        "a fresh store lists nothing, but must answer: {headers:?}"
    );
}
