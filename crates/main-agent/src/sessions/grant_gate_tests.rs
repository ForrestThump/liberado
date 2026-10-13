//! Session grant versus the process grant at the risk gate.
//!
//! These tests live beside `grants.rs` so that file stays at its function count.
//! The helpers return `Result` and use `?`. This file is still measured for
//! module health, and a helper `unwrap` or `expect` would count as process-fatal
//! if a later rename put it back on the production scan.

use super::super::*;
use super::test_fixtures::*;

/// Process grant is `tasks-mcp` only. The named session grant is `email-mcp` only.
/// The sets are disjoint, so a gate that still reads the process grant cannot both
/// show `email-mcp:send` and allow the call.
async fn disjoint_grant_sessions(
    dir: &std::path::Path,
    live_catalog: bool,
) -> Result<(ChatSessions, Ulid), String> {
    let store = Arc::new(SessionStore::open(dir).await);
    let executor = Executor::new(
        Arc::new(MockProvider::with_script(
            "mock",
            Vec::<CompletionResponse>::new(),
        )),
        Budget::default(),
    );
    let process =
        CapabilitySet::from_iter([liberado_common::Capability::ExecuteMcp("tasks-mcp".into())]);
    let mut sessions = ChatSessions::new(store, executor, Arc::new(TwoMcpTools)).with_guards(
        vec![
            ("tasks-mcp".into(), Consequence::ReadOnly),
            ("email-mcp".into(), Consequence::ReadOnly),
        ],
        process,
        dir.join("proposals"),
        ProposalSigner::random(),
    );
    if live_catalog {
        sessions = sessions.with_live_catalog(Arc::new(CapabilityCatalog::new()));
    }
    let id = sessions
        .create_with_grant(
            None,
            SessionGrant {
                capabilities: CapabilitySet::from_iter([liberado_common::Capability::ExecuteMcp(
                    "email-mcp".into(),
                )]),
                profile: Some("session-only".into()),
                ..Default::default()
            },
        )
        .await
        .map_err(|err| err.to_string())?;
    Ok((sessions, id))
}

/// Visibility alone already followed the session grant. The invoke must too:
/// `ScopedRuntime` would refuse a process-only tool with "not in scope" even when
/// the gate still held the process grant. The refusal string is the gate's.
async fn assert_session_tool_allowed_and_process_tool_refused(
    runtime: &dyn ToolRuntime,
) -> Result<(), String> {
    let mut saw_session = false;
    let mut saw_process = false;
    for tool in runtime.catalog() {
        if tool.name == "email-mcp:send" {
            saw_session = true;
        }
        if tool.name == "tasks-mcp:add" {
            saw_process = true;
        }
    }
    assert!(
        saw_session,
        "the session grant's tool must be visible on this session"
    );
    assert!(
        !saw_process,
        "a tool granted only to the process must stay invisible on this session"
    );

    let allowed = runtime
        .invoke(&ToolInvocation::new(
            "c-session",
            "email-mcp:send",
            serde_json::json!({}),
        ))
        .await
        .map_err(|err| {
            format!("a tool granted only to the session must pass the risk gate: {err}")
        })?;
    assert_eq!(allowed, "ok");

    let refused = match runtime
        .invoke(&ToolInvocation::new(
            "c-process",
            "tasks-mcp:add",
            serde_json::json!({}),
        ))
        .await
    {
        Err(err) => err,
        Ok(value) => {
            return Err(format!(
                "a tool granted only to the process must be refused, got {value}"
            ));
        }
    };
    assert!(
        refused.contains("not in the granted capability set"),
        "the risk gate must refuse the process-only tool as ungranted: {refused}"
    );
    Ok(())
}

#[tokio::test]
async fn scoped_extras_gates_on_the_session_grant_not_the_process_grant() -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|err| err.to_string())?;
    let (sessions, id) = disjoint_grant_sessions(dir.path(), false).await?;
    let grant = sessions.turn_settings(id).await.capabilities;
    let runtime = sessions.scoped_extras_runtime("list mail", id, grant);
    assert_session_tool_allowed_and_process_tool_refused(runtime.as_ref()).await
}

#[tokio::test]
async fn build_turn_runtime_gates_on_the_session_grant_not_the_process_grant() -> Result<(), String>
{
    let dir = tempfile::tempdir().map_err(|err| err.to_string())?;
    // Live catalog arms the same helper's other branch. An empty catalog does not
    // change the capability check; it only proves that branch still uses the session grant.
    let (sessions, id) = disjoint_grant_sessions(dir.path(), true).await?;
    let grant = sessions.turn_settings(id).await.capabilities;
    let runtime = sessions.build_turn_runtime("list mail", id, &[], &grant);
    assert_session_tool_allowed_and_process_tool_refused(runtime.as_ref()).await
}
