use liberado_common::UtcStamp;
use liberado_conversation_store::{
    Author, ChatMetaUpdate, ConversationStore, NewConversation, NewNode,
};
use liberado_provider::Message;
use liberado_session::Visibility;

use super::*;
use crate::{NewSession, SessionStore};

fn sample(id: &str, name: &str) -> UserProject {
    let now = UtcStamp::now();
    UserProject {
        id: id.into(),
        name: name.into(),
        instructions: None,
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn a_bare_array_migrates_to_the_current_schema() {
    let raw = serde_json::to_string(&vec![sample("01PROJECT", "Trip")]).unwrap();
    let (projects, rewritten) = migrate_catalog_json(&raw).unwrap();
    assert!(rewritten);
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].name, "Trip");
    assert_eq!(USER_PROJECTS_SCHEMA, 1);
}

#[test]
fn a_versionless_object_is_rewritten_and_a_current_file_is_not() {
    let legacy = r#"{"projects":[]}"#;
    let (_, rewritten) = migrate_catalog_json(legacy).unwrap();
    assert!(rewritten);

    let current = format!(r#"{{"schema_version":{USER_PROJECTS_SCHEMA},"projects":[]}}"#);
    let (_, rewritten) = migrate_catalog_json(&current).unwrap();
    assert!(!rewritten);
}

#[test]
fn a_newer_catalog_is_refused() {
    let raw = r#"{"schema_version":99,"projects":[]}"#;
    let error = migrate_catalog_json(raw).unwrap_err();
    assert!(error.contains("99"));
    assert!(error.contains(&USER_PROJECTS_SCHEMA.to_string()));
}

#[tokio::test]
async fn create_persists_and_reloads_without_a_coding_root() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).await;
    let created = store
        .create_user_project("  Trip notes  ", Some("  pack light  "))
        .await
        .unwrap();
    assert_eq!(created.name, "Trip notes");
    assert_eq!(created.instructions.as_deref(), Some("pack light"));
    drop(store);

    let reloaded = SessionStore::open(dir.path()).await;
    let listed = reloaded.list_user_projects().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);
    let file = std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
    assert!(file.contains(&format!("\"schema_version\": {USER_PROJECTS_SCHEMA}")));
    assert!(!file.contains("write_class"));
    assert!(!file.contains("\"root\""));
}

#[tokio::test]
async fn blank_and_duplicate_names_are_rejected() {
    let store = SessionStore::new();
    let error = store.create_user_project("   ", None).await.unwrap_err();
    assert!(matches!(error, ProjectWriteError::Invalid(_)));
    store.create_user_project("Trip", None).await.unwrap();
    let error = store.create_user_project("trip", None).await.unwrap_err();
    assert!(matches!(error, ProjectWriteError::Invalid(_)));
    let error = store
        .create_user_project("bad/name", None)
        .await
        .unwrap_err();
    assert!(matches!(error, ProjectWriteError::Invalid(_)));
}

#[tokio::test]
async fn rename_keeps_the_id_chats_are_stored_under() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).await;
    let project = store.create_user_project("Trip", None).await.unwrap();
    let header = store
        .create(NewConversation {
            title: Some("Packing".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    store
        .set_chat_meta(
            header.id,
            ChatMetaUpdate {
                project: Some(Some(project.id.clone())),
                pending_model: None,
            },
        )
        .await
        .unwrap();

    let renamed = store
        .update_user_project(&project.id, Some("Holiday"), None)
        .await
        .unwrap();
    assert_eq!(renamed.id, project.id);
    assert_eq!(renamed.name, "Holiday");
    let still = store.header(header.id).await.unwrap();
    assert_eq!(still.project.as_deref(), Some(project.id.as_str()));
}

#[tokio::test]
async fn delete_detaches_chats_and_leaves_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).await;
    let project = store.create_user_project("Trip", None).await.unwrap();
    let header = store
        .create(NewConversation {
            title: Some("Packing".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    store
        .set_chat_meta(
            header.id,
            ChatMetaUpdate {
                project: Some(Some(project.id.clone())),
                pending_model: None,
            },
        )
        .await
        .unwrap();
    let path = dir.path().join(format!("{}.jsonl", header.id));
    assert!(path.is_file());

    let detached = store.delete_user_project(&project.id).await.unwrap();
    assert_eq!(detached, 1);
    assert!(store.user_project(&project.id).await.is_none());
    let kept = store.header(header.id).await.unwrap();
    assert!(kept.project.is_none());
    assert_eq!(kept.title.as_deref(), Some("Packing"));
    assert!(path.is_file());
    let missing = store.delete_user_project(&project.id).await.unwrap_err();
    assert_eq!(missing, ProjectWriteError::NotFound);
}

#[tokio::test]
async fn project_chats_use_the_latest_message_and_skip_background() {
    let store = SessionStore::new();
    let project = store.create_user_project("Trip", None).await.unwrap();
    let older = store
        .create(NewConversation {
            title: Some("Older".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let newer = store
        .create(NewConversation {
            title: Some("Newer".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    for id in [older.id, newer.id] {
        store
            .set_chat_meta(
                id,
                ChatMetaUpdate {
                    project: Some(Some(project.id.clone())),
                    pending_model: None,
                },
            )
            .await
            .unwrap();
    }
    store
        .append(
            newer.id,
            NewNode {
                parent_id: None,
                author: Author::User,
                message: Message::user("later"),
                model: None,
                reasoning: None,
            },
        )
        .await
        .unwrap();
    let background = store
        .create_session(NewSession {
            title: Some("hidden".into()),
            visibility: Visibility::Background,
            project: Some(project.id.clone()),
            ..Default::default()
        })
        .await;
    assert_eq!(background.project.as_deref(), Some(project.id.as_str()));

    let rows = store.project_chats(&project.id).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].title.as_deref(), Some("Newer"));
    assert_eq!(rows[1].title.as_deref(), Some("Older"));
    assert!(rows[0].updated_at >= rows[1].updated_at);
}

#[tokio::test]
async fn an_unreadable_catalog_is_not_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    std::fs::write(&path, "{not json").unwrap();
    let store = SessionStore::open(dir.path()).await;
    let error = store.create_user_project("Trip", None).await.unwrap_err();
    assert!(matches!(error, ProjectWriteError::Unavailable(_)));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
}
