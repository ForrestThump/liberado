use std::str::FromStr;
use std::sync::Arc;

use liberado_bootstrap::Config;
use liberado_executor::ToolRuntime;
use liberado_provider::ToolInvocation;

use super::{
    LiveSurfaces, PluginAccess, PluginReject, ProfileCatalog, ProfileReject, RenamePlan,
    is_protected,
};

fn config() -> Arc<Config> {
    Arc::new(
        Config::from_str(
            r#"
[topology]
vault_path = "/tmp/vault"

[[topology.session_profiles]]
name = "coding"

[[topology.session_profiles]]
name = "life"

[[topology.session_profiles]]
name = "chat-default"

[[topology.session_profiles]]
name = "operator"

[[policy.grants]]
component = "coding"
capabilities = []

[[policy.grants]]
component = "life"
capabilities = []

[[policy.grants]]
component = "chat-default"
capabilities = []

[[policy.grants]]
component = "operator"
capabilities = []
"#,
        )
        .unwrap(),
    )
}

#[test]
fn rename_keeps_the_topology_grant_under_the_new_name() {
    let profiles = ProfileCatalog::memory(config());
    let plan = profiles.plan_rename("coding", "builder").unwrap();
    let RenamePlan::Apply { original } = plan else {
        panic!("expected a rename");
    };
    profiles.commit_rename(&original, "builder").unwrap();
    let resolved = profiles.resolve(Some("builder"), "").unwrap();
    assert_eq!(resolved.name.as_deref(), Some("builder"));
    assert!(profiles.resolve(Some("coding"), "").is_err());
    let body = profiles.list_json();
    let names: Vec<&str> = body["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["name"].as_str())
        .collect();
    assert!(names.contains(&"builder"));
    assert!(!names.contains(&"coding"));
}

#[test]
fn built_ins_and_collisions_and_unknowns_are_refused() {
    let profiles = ProfileCatalog::memory(config());
    assert_eq!(
        profiles.plan_rename("operator", "other"),
        Err(ProfileReject::Protected)
    );
    assert_eq!(
        profiles.plan_delete("chat-default"),
        Err(ProfileReject::Protected)
    );
    assert!(is_protected("operator"));
    assert!(is_protected("chat-default"));
    assert_eq!(profiles.plan_delete("missing"), Err(ProfileReject::Unknown));
    assert_eq!(
        profiles.plan_rename("coding", "life"),
        Err(ProfileReject::Collision)
    );
    assert_eq!(
        profiles.plan_rename("coding", "has space"),
        Err(ProfileReject::Invalid(
            "Use letters, digits, and . _ - in the profile name."
        ))
    );
    assert!(matches!(
        profiles.plan_rename("coding", "coding"),
        Ok(RenamePlan::Unchanged)
    ));
}

#[test]
fn delete_hides_the_name_and_the_ledger_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let profiles = ProfileCatalog::open(config(), dir.path());
    let plan = profiles.plan_delete("life").unwrap();
    profiles.commit_delete(&plan.original).unwrap();
    assert!(profiles.resolve(Some("life"), "").is_err());
    let again = ProfileCatalog::open(config(), dir.path());
    assert!(again.resolve(Some("life"), "").is_err());
    assert!(again.resolve(Some("coding"), "").is_ok());
}

#[test]
fn plugin_switch_cannot_widen_and_persists_a_narrowing() {
    let dir = tempfile::tempdir().unwrap();
    let access = PluginAccess::open(dir.path());
    assert_eq!(
        access.set_chat_enabled("tasks", true, false),
        Err(PluginReject::NotGranted)
    );
    assert!(!access.chat_visible("tasks", false));
    assert!(!access.set_chat_enabled("tasks", false, true).unwrap());
    assert!(!access.chat_visible("tasks", true));
    let again = PluginAccess::open(dir.path());
    assert!(!again.chat_visible("tasks", true));
    assert!(again.set_chat_enabled("tasks", true, true).unwrap());
    assert!(again.chat_visible("tasks", true));
}

struct OneTool;

#[async_trait::async_trait]
impl ToolRuntime for OneTool {
    fn catalog(&self) -> Vec<liberado_provider::ToolDef> {
        vec![liberado_provider::ToolDef::new(
            "tasks:add",
            "add",
            serde_json::json!({}),
        )]
    }

    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        Ok("ran".into())
    }
}

#[tokio::test]
async fn the_chat_mask_hides_and_refuses_a_switched_off_server() {
    let access = PluginAccess::memory();
    access.set_chat_enabled("tasks", false, true).unwrap();
    let runtime = super::mask_chat_runtime(Arc::new(OneTool), access);
    assert!(runtime.catalog().is_empty());
    let call = ToolInvocation::new("1", "tasks:add", serde_json::json!({}));
    let err = runtime.invoke(&call).await.unwrap_err();
    assert!(err.contains("tasks"), "{err}");
}

#[test]
fn inert_surfaces_share_no_ledger_file() {
    let surfaces = LiveSurfaces::inert();
    assert!(surfaces.profiles.resolve(None, "life").is_ok());
}
