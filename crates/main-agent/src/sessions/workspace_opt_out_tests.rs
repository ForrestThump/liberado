//! A profile can keep the private file tools off its turn.

use super::super::*;
use super::test_fixtures::*;

use std::collections::HashMap;
use std::sync::Arc;

struct Held {
    _dir: tempfile::TempDir,
    _files: tempfile::TempDir,
    sessions: ChatSessions,
    id: Ulid,
    provider: Arc<MockProvider>,
}

fn catalog_names(runtime: &dyn ToolRuntime) -> Vec<String> {
    runtime
        .catalog()
        .into_iter()
        .map(|tool| tool.name)
        .collect()
}

fn offered_names(provider: &MockProvider) -> Vec<String> {
    provider.received_requests()[0]
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect()
}

fn workspace_offered(names: &[String]) -> bool {
    crate::WORKSPACE_TOOL_NAMES
        .iter()
        .any(|name| names.iter().any(|got| got == name))
}

async fn held(workspace: bool) -> Held {
    let dir = tempfile::tempdir().expect("store dir");
    let files = tempfile::tempdir().expect("workspace dir");
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "m",
        vec![CompletionResponse::text("ok")],
    ));
    let sessions = ChatSessions::new(
        store,
        Executor::new(provider.clone(), Budget::default()),
        Arc::new(OneTool("budget-mcp:list")),
    )
    .with_guards(
        vec![("budget-mcp".into(), Consequence::ReadOnly)],
        CapabilitySet::from_iter([Capability::ExecuteMcp("budget-mcp".into())]),
        dir.path().join("proposals"),
        ProposalSigner::random(),
    )
    .with_agent_workspace(liberado_agent_workspace::WorkspaceSettings {
        root: files.path().to_path_buf(),
        max_bytes: liberado_agent_workspace::DEFAULT_CAP_BYTES,
    });
    let id = sessions
        .create_with_grant(
            None,
            SessionGrant {
                capabilities: CapabilitySet::from_iter([Capability::ExecuteMcp(
                    "budget-mcp".into(),
                )]),
                profile: Some("budget-bot".into()),
                delegation: Some(false),
                workspace,
                ..Default::default()
            },
        )
        .await
        .expect("session");
    Held {
        _dir: dir,
        _files: files,
        sessions,
        id,
        provider,
    }
}

async fn runtime_for(held: &Held) -> Box<dyn ToolRuntime> {
    let settings = held.sessions.turn_settings(held.id).await;
    held.sessions.build_turn_runtime(
        "list the files",
        held.id,
        &[],
        &settings.capabilities,
        settings.profile.as_deref(),
        settings.workspace,
    )
}

#[tokio::test]
async fn workspace_false_omits_file_tools_and_the_gate_refuses_them() {
    let held = held(false).await;
    let runtime = runtime_for(&held).await;
    let names = catalog_names(runtime.as_ref());
    assert!(
        !workspace_offered(&names),
        "opt-out catalog must not list file tools: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "budget-mcp:list"),
        "the granted MCP tool stays on the turn: {names:?}"
    );
    let refused = runtime
        .invoke(&ToolInvocation::new(
            "w",
            "workspace_list",
            serde_json::json!({ "path": "." }),
        ))
        .await
        .expect_err("a withheld file tool must not run");
    assert!(
        refused.contains("not in the granted capability set"),
        "the risk gate must refuse the withheld tool: {refused}"
    );
    let allowed = runtime
        .invoke(&ToolInvocation::new(
            "m",
            "budget-mcp:list",
            serde_json::json!({}),
        ))
        .await
        .expect("the granted tool still passes the gate");
    assert_eq!(allowed, "ok");

    held.sessions
        .turn(held.id, "list the files")
        .await
        .expect("turn");
    let offered = offered_names(&held.provider);
    assert!(
        !workspace_offered(&offered),
        "the model request must not carry file tools: {offered:?}"
    );
    assert!(
        offered.iter().any(|name| name == "budget-mcp:list"),
        "the model request keeps the granted tool: {offered:?}"
    );
}

#[tokio::test]
async fn workspace_true_still_offers_the_file_tools() {
    let held = held(true).await;
    let runtime = runtime_for(&held).await;
    let names = catalog_names(runtime.as_ref());
    for name in crate::WORKSPACE_TOOL_NAMES {
        assert!(
            names.iter().any(|got| got == name),
            "default grant must offer {name}: {names:?}"
        );
    }
    let listed = runtime
        .invoke(&ToolInvocation::new(
            "w",
            "workspace_list",
            serde_json::json!({ "path": "." }),
        ))
        .await
        .expect("file tools run in front of the risk gate");
    assert!(listed.contains("private"), "{listed}");

    held.sessions
        .turn(held.id, "list the files")
        .await
        .expect("turn");
    let offered = offered_names(&held.provider);
    for name in crate::WORKSPACE_TOOL_NAMES {
        assert!(
            offered.iter().any(|got| got == name),
            "the model request must offer {name}: {offered:?}"
        );
    }
}

#[tokio::test]
async fn a_live_profile_flag_hides_file_tools_on_an_old_grant() {
    let held = held(true).await;
    let mut flags = HashMap::new();
    flags.insert("budget-bot".into(), false);
    let sessions = held.sessions.with_profile_workspaces(flags);
    let settings = sessions.turn_settings(held.id).await;
    assert!(!settings.workspace);
    let runtime = sessions.build_turn_runtime(
        "list the files",
        held.id,
        &[],
        &settings.capabilities,
        settings.profile.as_deref(),
        settings.workspace,
    );
    let names = catalog_names(runtime.as_ref());
    assert!(
        !workspace_offered(&names),
        "the loaded profile must override a stored grant that still says on: {names:?}"
    );
}
