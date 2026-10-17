use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use liberado_common::{Capability, CapabilitySet, Proposal, ProposalSigner};
use liberado_conversation_store::{Author, is_agent_creator_profile};
use liberado_executor::{Budget, Executor};
use liberado_provider::MockProvider;
use liberado_session::SessionGrant;
use liberado_session_store::SessionStore;
use liberado_test_support::NoopRuntime;
use serde_json::json;

use super::super::TurnSettings;
use super::{
    AUTOMATION_EDITOR_OPENER, AUTOMATION_EDITOR_PROFILE, AUTOMATION_EDITOR_SYSTEM_PROMPT,
    EditorTarget, SCHEDULE_PROPOSAL_ACK, editor_chat_title, editor_system_prompt,
    editor_target_context,
};
use crate::ChatSessions;
use crate::face::{
    CREATE_SCHEDULE_TOOL_NAME, DELETE_SCHEDULE_TOOL_NAME, LIST_SCHEDULES_TOOL_NAME,
    ScheduleEditorOps, UPDATE_SCHEDULE_TOOL_NAME, is_schedule_write_tool,
};

fn sessions(store: Arc<SessionStore>) -> Arc<ChatSessions> {
    let sessions = Arc::new(
        ChatSessions::new(
            store,
            Executor::new(
                Arc::new(MockProvider::with_script("m", vec![])),
                Budget::default(),
            ),
            Arc::new(NoopRuntime),
        )
        .with_profile_resolver(Arc::new(|name: &str| {
            Ok(SessionGrant {
                profile: Some(name.to_owned()),
                delegation: Some(false),
                ..SessionGrant::default()
            })
        })),
    );
    sessions.install_self_handle();
    sessions
}

fn settings(editor: bool) -> TurnSettings {
    TurnSettings {
        capabilities: CapabilitySet::empty(),
        delegation: false,
        prompt_append: None,
        profile: Some(AUTOMATION_EDITOR_PROFILE.into()),
        model: None,
        automation_editor: editor,
    }
}

#[test]
fn the_editor_profile_is_not_an_agent_creator() {
    assert_eq!(
        AUTOMATION_EDITOR_PROFILE,
        liberado_config_loader::BUILTIN_AUTOMATION_EDITOR_PROFILE
    );
    assert!(!is_agent_creator_profile(Some(AUTOMATION_EDITOR_PROFILE)));
    assert!(is_agent_creator_profile(Some("operator")));
}

#[tokio::test]
async fn editor_catalog_is_the_four_tools_and_another_profile_does_not_see_them() {
    let store = Arc::new(SessionStore::new());
    let sessions = sessions(store);
    let id = sessions.open_automation_editor(None).await.unwrap();
    let editor = sessions.build_face_runtime(
        "user",
        id,
        CapabilitySet::empty(),
        Arc::new(AtomicBool::new(false)),
        &settings(true),
    );
    let names: Vec<_> = editor.catalog().into_iter().map(|tool| tool.name).collect();
    assert_eq!(
        names,
        vec![
            LIST_SCHEDULES_TOOL_NAME.to_string(),
            CREATE_SCHEDULE_TOOL_NAME.to_string(),
            UPDATE_SCHEDULE_TOOL_NAME.to_string(),
            DELETE_SCHEDULE_TOOL_NAME.to_string(),
        ]
    );
    let other = sessions.build_face_runtime(
        "user",
        id,
        CapabilitySet::empty(),
        Arc::new(AtomicBool::new(false)),
        &settings(false),
    );
    let other_names: Vec<_> = other.catalog().into_iter().map(|tool| tool.name).collect();
    assert!(
        other_names
            .iter()
            .all(|name| !is_schedule_write_tool(name) && name != LIST_SCHEDULES_TOOL_NAME),
        "{other_names:?}"
    );
}

#[test]
fn editor_prompt_quotes_central_time_and_keeps_cron_in_utc() {
    assert!(
        AUTOMATION_EDITOR_SYSTEM_PROMPT.contains(
            "Judge timing only from next_fires and the code-generated description; never from names, comments or labels; compare them to what the user asked and say so if they disagree."
        ),
        "{AUTOMATION_EDITOR_SYSTEM_PROMPT}"
    );
    assert!(
        AUTOMATION_EDITOR_SYSTEM_PROMPT.contains("next_fires"),
        "{AUTOMATION_EDITOR_SYSTEM_PROMPT}"
    );
    assert!(AUTOMATION_EDITOR_SYSTEM_PROMPT.contains("timezone"));
    assert!(AUTOMATION_EDITOR_SYSTEM_PROMPT.contains("UTC"));
    assert!(AUTOMATION_EDITOR_SYSTEM_PROMPT.contains("Central time"));
    assert!(AUTOMATION_EDITOR_SYSTEM_PROMPT.contains("service restarts"));
    assert!(AUTOMATION_EDITOR_SYSTEM_PROMPT.contains("first assistant message"));
    assert!(AUTOMATION_EDITOR_SYSTEM_PROMPT.contains("already given"));
    assert!(
        AUTOMATION_EDITOR_SYSTEM_PROMPT.contains("Do not present the UTC hour as the local time")
    );
    assert_eq!(
        AUTOMATION_EDITOR_OPENER,
        "What automation changes do you want to make?"
    );
}

fn sample_target() -> EditorTarget {
    EditorTarget {
        name: "morning".into(),
        enabled: true,
        schedule_text: "Every day at 6:30 AM".into(),
        cron_expr: "0 30 12 * * * *".into(),
        timezone: "America/Chicago".into(),
        description: "Sends a reminder listing calendar events.".into(),
    }
}

#[test]
fn edit_context_names_the_schedule_and_new_has_no_seed() {
    let target = sample_target();
    let context = editor_target_context(&target);
    assert!(context.contains("Name (id): morning."));
    assert!(context.contains("Every day at 6:30 AM"));
    assert!(context.contains("0 30 12 * * * *"));
    assert!(context.contains("America/Chicago"));
    assert!(context.contains("Sends a reminder listing calendar events."));
    assert!(context.contains("already given"));
    let seeded = editor_system_prompt(Some(&target));
    assert!(seeded.contains("Name (id): morning."));
    assert!(seeded.starts_with(AUTOMATION_EDITOR_SYSTEM_PROMPT));
    let fresh = editor_system_prompt(None);
    assert!(!fresh.contains("Name (id):"));
    assert_eq!(editor_chat_title(None), "New automation");
    assert_eq!(editor_chat_title(Some(&target)), "Edit morning");
}

#[tokio::test]
async fn each_open_is_a_new_chat_and_seeds_one_opener() {
    let store = Arc::new(SessionStore::new());
    let sessions = sessions(store);
    let id = sessions.open_automation_editor(None).await.unwrap();
    let again = sessions
        .open_automation_editor(Some(sample_target()))
        .await
        .unwrap();
    assert_ne!(id, again);
    let header = sessions.store.header(id).await.unwrap();
    assert!(header.automation_editor);
    assert!(!header.agent_creator);
    assert_eq!(header.title.as_deref(), Some("New automation"));
    assert_eq!(
        header.grant.profile.as_deref(),
        Some(AUTOMATION_EDITOR_PROFILE)
    );
    assert_eq!(
        header.surface_mode,
        liberado_conversation_store::SurfaceMode::Chat
    );
    let nodes = sessions.store.leaf_path(id, None).await.unwrap();
    let visible: Vec<_> = nodes
        .iter()
        .filter(|node| !matches!(node.author, Author::System))
        .collect();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].message.content, AUTOMATION_EDITOR_OPENER);
    let edit = sessions.store.header(again).await.unwrap();
    assert_eq!(edit.title.as_deref(), Some("Edit morning"));
    let edit_nodes = sessions.store.leaf_path(again, None).await.unwrap();
    let edit_visible: Vec<_> = edit_nodes
        .iter()
        .filter(|node| !matches!(node.author, Author::System))
        .collect();
    assert_eq!(edit_visible.len(), 1);
    assert_eq!(edit_visible[0].message.content, AUTOMATION_EDITOR_OPENER);
    let system = edit_nodes
        .iter()
        .find(|node| matches!(node.author, Author::System))
        .unwrap();
    assert!(system.message.content.contains("Name (id): morning."));
    assert!(!system.message.content.contains(AUTOMATION_EDITOR_OPENER));
}

#[tokio::test]
async fn a_proposal_does_not_touch_topology_before_approval() {
    let proposals = tempfile::tempdir().unwrap();
    let topology_dir = tempfile::tempdir().unwrap();
    let topology = topology_dir.path().join("topology.toml");
    let original = "name = \"keep\"\n# comment\n";
    std::fs::write(&topology, original).unwrap();
    let store = Arc::new(SessionStore::new());
    let sessions = Arc::new(
        ChatSessions::new(
            store,
            Executor::new(
                Arc::new(MockProvider::with_script("m", vec![])),
                Budget::default(),
            ),
            Arc::new(NoopRuntime),
        )
        .with_guards(
            Vec::new(),
            CapabilitySet::empty(),
            proposals.path().to_path_buf(),
            ProposalSigner::random(),
        ),
    );
    for tool in [
        CREATE_SCHEDULE_TOOL_NAME,
        UPDATE_SCHEDULE_TOOL_NAME,
        DELETE_SCHEDULE_TOOL_NAME,
    ] {
        let ack = sessions
            .propose_edit(
                "sess-1",
                tool,
                json!({"name": "morning", "cron_expr": "not-a-cron"}),
                "edit morning",
            )
            .await
            .unwrap();
        assert_eq!(ack, SCHEDULE_PROPOSAL_ACK);
        assert_eq!(std::fs::read_to_string(&topology).unwrap(), original);
    }
    let mut notes = 0;
    for entry in std::fs::read_dir(proposals.path().join("proposals")).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        let proposal = Proposal::from_note(&text).unwrap();
        let liberado_common::ProposedAction::ToolCalls(calls) = &proposal.proposed_action else {
            panic!("schedule card must carry a tool call: {text}");
        };
        let tool_name = &calls[0].tool;
        assert_eq!(
            proposal.requested_grant,
            Some(Capability::ExecuteTool(format!(
                "{AUTOMATION_EDITOR_PROFILE}:{tool_name}"
            ))),
            "{text}"
        );
        assert_eq!(proposal.session_id.as_deref(), Some("sess-1"));
        assert_eq!(proposal.origin, Some(liberado_common::ApprovalOrigin::Web));
        notes += 1;
    }
    assert_eq!(notes, 3);
    assert_eq!(std::fs::read_to_string(&topology).unwrap(), original);
}

#[tokio::test]
async fn a_proposal_ack_includes_the_code_computed_timing() {
    let proposals = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::new());
    let sessions = Arc::new(
        ChatSessions::new(
            store,
            Executor::new(
                Arc::new(MockProvider::with_script("m", vec![])),
                Budget::default(),
            ),
            Arc::new(NoopRuntime),
        )
        .with_guards(
            Vec::new(),
            CapabilitySet::empty(),
            proposals.path().to_path_buf(),
            ProposalSigner::random(),
        )
        .with_schedule_facts(Arc::new(|tool, args| {
            format!(
                "cron_expr: {}\nschedule_text: Every Tuesday at 8:00 PM\nnext_fires: Tue Oct 20",
                args["cron_expr"].as_str().unwrap_or(tool)
            )
        })),
    );
    let ack = sessions
        .propose_edit(
            "sess-1",
            UPDATE_SCHEDULE_TOOL_NAME,
            json!({"name": "trash", "cron_expr": "0 0 1 * * 4 *"}),
            "move trash night",
        )
        .await
        .unwrap();
    assert!(ack.starts_with(SCHEDULE_PROPOSAL_ACK), "{ack}");
    assert!(ack.contains("cron_expr: 0 0 1 * * 4 *"), "{ack}");
    assert!(
        ack.contains("schedule_text: Every Tuesday at 8:00 PM"),
        "{ack}"
    );
    assert!(ack.contains("next_fires: Tue Oct 20"), "{ack}");
}

fn editor_policy_without_a_topology_row() -> liberado_config_loader::Config {
    let mut cfg = liberado_config_loader::Config::default();
    cfg.topology.vault_path = std::path::PathBuf::from("/tmp/vault");
    cfg.policy.grants = vec![
        liberado_config_loader::Grant {
            component: "main-agent".into(),
            capabilities: vec![Capability::AskHuman],
        },
        liberado_config_loader::Grant {
            component: AUTOMATION_EDITOR_PROFILE.into(),
            capabilities: vec![Capability::ExecuteTool(
                "automation-editor:list_schedules".into(),
            )],
        },
    ];
    cfg
}

fn grant_for_named_hat(
    cfg: &liberado_config_loader::Config,
    name: &str,
) -> Result<SessionGrant, String> {
    let resolved = cfg
        .resolve_session_profile(Some(name), "main-agent")
        .map_err(|err| err.to_string())?;
    let parts = resolved.grant_parts();
    let overrides = serde_json::to_value(&resolved.overrides).map_err(|err| err.to_string())?;
    Ok(SessionGrant {
        capabilities: parts.capabilities,
        profile: parts.profile,
        overrides,
        delegation: parts.delegation,
        model: parts.model.map(str::to_string),
        prompt_append: parts.prompt_append.map(str::to_string),
    })
}

#[tokio::test]
async fn a_later_editor_turn_keeps_schedule_tools_and_the_policy_grant() {
    let cfg = editor_policy_without_a_topology_row();
    let store = Arc::new(SessionStore::new());
    let sessions = Arc::new(
        ChatSessions::new(
            store,
            Executor::new(
                Arc::new(MockProvider::with_script("m", vec![])),
                Budget::default(),
            ),
            Arc::new(NoopRuntime),
        )
        .with_profile_resolver(Arc::new({
            let cfg = cfg.clone();
            move |name: &str| grant_for_named_hat(&cfg, name)
        })),
    );
    sessions.install_self_handle();
    let id = sessions.open_automation_editor(None).await.unwrap();

    let first = sessions.turn_settings(id).await;
    let listed = Capability::ExecuteTool("automation-editor:list_schedules".into());
    assert!(first.automation_editor);
    assert!(!first.delegation);
    assert_eq!(first.profile.as_deref(), Some(AUTOMATION_EDITOR_PROFILE));
    assert_eq!(first.capabilities.capabilities, vec![listed]);
    assert!(!first.capabilities.grants_ask_human());

    let runtime = sessions.build_face_runtime(
        "list them",
        id,
        first.capabilities.clone(),
        Arc::new(AtomicBool::new(false)),
        &first,
    );
    let names: Vec<_> = runtime
        .catalog()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(
        names,
        vec![
            LIST_SCHEDULES_TOOL_NAME.to_string(),
            CREATE_SCHEDULE_TOOL_NAME.to_string(),
            UPDATE_SCHEDULE_TOOL_NAME.to_string(),
            DELETE_SCHEDULE_TOOL_NAME.to_string(),
        ]
    );

    let second = sessions.turn_settings(id).await;
    assert_eq!(second.capabilities, first.capabilities);
    assert!(second.automation_editor);
    assert!(!second.delegation);
    let again = cfg
        .resolve_session_profile(Some(AUTOMATION_EDITOR_PROFILE), "main-agent")
        .unwrap();
    assert_eq!(again.capabilities, first.capabilities);
    assert_eq!(again.delegation, Some(false));
}
