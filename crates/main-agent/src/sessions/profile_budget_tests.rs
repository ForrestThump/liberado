//! A profile max_turns replaces the face budget. An unset profile keeps it.

use std::collections::HashMap;
use std::sync::Arc;

use liberado_common::CapabilitySet;
use liberado_executor::Budget;
use liberado_provider::{CompletionResponse, ToolInvocation};
use liberado_session::SessionGrant;

use super::super::{ChatSessions, TurnSettings};
use super::test_fixtures::*;

fn settings(profile: Option<&str>) -> TurnSettings {
    TurnSettings {
        capabilities: CapabilitySet::empty(),
        delegation: false,
        prompt_append: None,
        profile: profile.map(str::to_string),
        model: None,
        automation_editor: false,
    }
}

fn tool(name: &str) -> CompletionResponse {
    CompletionResponse::tool_calls(vec![ToolInvocation::new(
        name,
        name,
        serde_json::json!({ "n": name }),
    )])
}

#[tokio::test]
async fn a_profile_ceiling_overrides_the_face_budget_and_an_omitted_one_does_not() {
    let dir = tempfile::tempdir().expect("dir");
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "face",
        vec![
            tool("one"),
            tool("two"),
            tool("three"),
            tool("four"),
            CompletionResponse::text("done"),
        ],
    ));
    let sessions = ChatSessions::new(
        store,
        liberado_executor::Executor::new(provider, Budget::new(12)),
        Arc::new(no_tools_runtime()),
    )
    .with_profile_max_turns(HashMap::from([("budget-bot".into(), 2)]));

    assert_eq!(sessions.executor_for_turn(&settings(None)).max_turns(), 12);
    assert_eq!(
        sessions
            .executor_for_turn(&settings(Some("budget-bot")))
            .max_turns(),
        2
    );
    assert_eq!(
        sessions
            .executor_for_turn(&settings(Some("chat-default")))
            .max_turns(),
        12
    );

    let limited = sessions
        .create_with_grant(
            None,
            SessionGrant {
                profile: Some("budget-bot".into()),
                ..SessionGrant::default()
            },
        )
        .await
        .expect("session");
    let err = sessions.turn(limited, "what is due").await.unwrap_err();
    let err = err.to_string();
    assert!(
        err.contains("exceeded its turns budget"),
        "profile ceiling must stop the loop: {err}"
    );

    let open = sessions.create(None).await.expect("session");
    let reply = sessions.turn(open, "what is due").await.expect("reply");
    assert_eq!(reply, "done");

    sessions.rename_profile_max_turns("budget-bot", "finance");
    assert_eq!(
        sessions
            .executor_for_turn(&settings(Some("finance")))
            .max_turns(),
        2
    );
    assert_eq!(
        sessions
            .executor_for_turn(&settings(Some("budget-bot")))
            .max_turns(),
        12
    );
    sessions.clear_profile_max_turns("finance");
    assert_eq!(
        sessions
            .executor_for_turn(&settings(Some("finance")))
            .max_turns(),
        12
    );
}
