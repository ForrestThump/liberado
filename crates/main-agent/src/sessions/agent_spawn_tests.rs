use super::*;
use liberado_conversation_store::{AgentProfiles, Author, ConversationStore, NewNode, Ulid};
use liberado_executor::{Budget, Executor};
use liberado_provider::{Message, MockProvider};
use liberado_session_store::SessionStore;
use liberado_test_support::NoopRuntime;

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
                ..SessionGrant::default()
            })
        })),
    );
    sessions.install_self_handle();
    sessions
}

fn id_of(result: &CreateAgentResult) -> Ulid {
    result.conversation_id.parse().unwrap()
}

#[test]
fn agent_identity_trims_and_defaults_a_blank_title_to_the_profile() {
    assert_eq!(agent_identity("  ", None), None);
    assert_eq!(agent_identity("", Some("Coder")), None);
    assert_eq!(
        agent_identity(" coding ", None),
        Some(("coding".into(), "coding".into()))
    );
    assert_eq!(
        agent_identity("coding", Some("   ")),
        Some(("coding".into(), "coding".into()))
    );
    assert_eq!(
        agent_identity("coding", Some("  Coder  ")),
        Some(("coding".into(), "Coder".into()))
    );
    assert_ne!(
        agent_identity("coding", Some("Coder")),
        agent_identity("coding", Some("coder"))
    );
}

#[test]
fn header_match_requires_projected_agent_and_skips_the_creator() {
    let mut header = ConversationHeader {
        title: Some("Coder".into()),
        surface_mode: SurfaceMode::Agent,
        grant: SessionGrant {
            profile: Some("coding".into()),
            ..SessionGrant::default()
        },
        ..ConversationHeader::default()
    };
    assert!(header_matches_agent(&header, "coding", "Coder"));
    header.agent_creator = true;
    assert!(!header_matches_agent(&header, "coding", "Coder"));
    header.agent_creator = false;
    header.surface_mode = SurfaceMode::Chat;
    assert!(!header_matches_agent(&header, "coding", "Coder"));
}

#[test]
fn pick_oldest_breaks_ties_on_id_after_created_at() {
    let early = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let late = chrono::DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let mut first = ConversationHeader {
        id: Ulid::from(1u128),
        created_at: late,
        ..ConversationHeader::default()
    };
    let second = ConversationHeader {
        id: Ulid::from(2u128),
        created_at: early,
        ..ConversationHeader::default()
    };
    assert_eq!(
        pick_oldest([&first, &second].into_iter()).unwrap().id,
        second.id
    );
    first.created_at = early;
    let third = ConversationHeader {
        id: Ulid::from(9u128),
        created_at: early,
        ..ConversationHeader::default()
    };
    assert_eq!(
        pick_oldest([&third, &first].into_iter()).unwrap().id,
        first.id
    );
}

#[tokio::test]
async fn open_agent_creator_is_a_singleton_with_a_canned_opener() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let sessions = sessions(store.clone());

    let (id, created) = sessions.open_agent_creator().await.unwrap();
    assert!(created);
    let (again, created_again) = sessions.open_agent_creator().await.unwrap();
    assert!(!created_again);
    assert_eq!(id, again);

    let header = store.header(id).await.unwrap();
    assert!(header.agent_creator);
    assert_eq!(header.surface_mode, SurfaceMode::Agent);
    assert_eq!(header.grant.profile.as_deref(), Some(AGENT_CREATOR_PROFILE));
    assert_eq!(header.title.as_deref(), Some(AGENT_CREATOR_TITLE));

    let nodes = sessions.history_nodes(id).await.unwrap();
    let system = nodes
        .iter()
        .find(|n| matches!(n.author, Author::System))
        .unwrap();
    assert!(system.message.content.contains("create_agent"));
    assert!(!system.message.content.contains("personal AI assistant"));
    let assistants: Vec<_> = nodes
        .iter()
        .filter(|n| matches!(n.author, Author::Assistant))
        .collect();
    assert_eq!(assistants.len(), 1);
    assert_eq!(assistants[0].message.content, AGENT_CREATOR_OPENER);
    assert!(assistants[0].model.is_none());

    store
        .append(
            id,
            NewNode {
                parent_id: nodes.last().map(|n| n.id),
                author: Author::User,
                message: Message::user("a coding agent"),
                model: None,
            },
        )
        .await
        .unwrap();
    let _ = sessions.open_agent_creator().await.unwrap();
    let after = sessions.history_nodes(id).await.unwrap();
    assert_eq!(
        after
            .iter()
            .filter(|n| matches!(n.author, Author::Assistant))
            .count(),
        1,
        "a later open must not re-seed the opener"
    );

    sessions
        .set_title(id, "Renamed creator".into())
        .await
        .unwrap();
    let (after_rename, _) = sessions.open_agent_creator().await.unwrap();
    assert_eq!(after_rename, id);
}

#[tokio::test]
async fn create_agent_chat_reuses_one_session_per_profile_and_title() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let sessions = sessions(store.clone());

    let first = sessions
        .create_agent_chat("coding", Some("  Coder  ".into()))
        .await
        .unwrap();
    assert!(!first.reused);
    assert_eq!(first.title.as_deref(), Some("Coder"));

    let again = sessions
        .create_agent_chat("coding", Some("Coder".into()))
        .await
        .unwrap();
    assert!(again.reused);
    assert_eq!(again.conversation_id, first.conversation_id);

    let blank = sessions.create_agent_chat("coding", None).await.unwrap();
    assert!(!blank.reused, "blank title is the profile name, not Coder");
    let blank_again = sessions
        .create_agent_chat("coding", Some("coding".into()))
        .await
        .unwrap();
    assert!(blank_again.reused);
    assert_eq!(blank_again.conversation_id, blank.conversation_id);

    let other_title = sessions
        .create_agent_chat("coding", Some("coder".into()))
        .await
        .unwrap();
    assert!(!other_title.reused);
    assert_ne!(other_title.conversation_id, first.conversation_id);

    let other_profile = sessions
        .create_agent_chat("researcher", Some("Coder".into()))
        .await
        .unwrap();
    assert!(!other_profile.reused);
    assert_ne!(other_profile.conversation_id, first.conversation_id);

    let rows = store.list().await.unwrap();
    let coders = rows
        .iter()
        .filter(|h| h.title.as_deref() == Some("Coder") && !h.agent_creator)
        .count();
    assert_eq!(coders, 2, "coding/Coder and researcher/Coder");
}

#[tokio::test]
async fn create_agent_chat_reuses_the_oldest_preexisting_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let sessions = sessions(store.clone());
    let grant = SessionGrant {
        profile: Some("life".into()),
        ..SessionGrant::default()
    };
    let older = sessions
        .create_with_grant(Some("Life".into()), grant.clone())
        .await
        .unwrap();
    let newer = sessions
        .create_with_grant(Some("Life".into()), grant)
        .await
        .unwrap();
    let result = sessions
        .create_agent_chat("life", Some("Life".into()))
        .await
        .unwrap();
    assert!(result.reused);
    assert_eq!(id_of(&result), older);
    assert_ne!(id_of(&result), newer);
    let matches = store
        .list()
        .await
        .unwrap()
        .into_iter()
        .filter(|h| header_matches_agent(h, "life", "Life"))
        .count();
    assert_eq!(matches, 2);
}

#[tokio::test]
async fn create_agent_does_not_reuse_the_creator_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let sessions = sessions(store);
    let (creator, _) = sessions.open_agent_creator().await.unwrap();
    let specialist = sessions
        .create_agent_chat("operator", Some(AGENT_CREATOR_TITLE.into()))
        .await
        .unwrap();
    assert!(!specialist.reused);
    assert_ne!(id_of(&specialist), creator);
}

#[tokio::test]
async fn renaming_a_specialist_retargets_its_identity() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let sessions = sessions(store);
    let first = sessions
        .create_agent_chat("coding", Some("Coder".into()))
        .await
        .unwrap();
    sessions
        .set_title(id_of(&first), "Renamed".into())
        .await
        .unwrap();
    let old_name = sessions
        .create_agent_chat("coding", Some("Coder".into()))
        .await
        .unwrap();
    assert!(!old_name.reused);
    assert_ne!(old_name.conversation_id, first.conversation_id);
    let new_name = sessions
        .create_agent_chat("coding", Some("Renamed".into()))
        .await
        .unwrap();
    assert!(new_name.reused);
    assert_eq!(new_name.conversation_id, first.conversation_id);
}

#[tokio::test]
async fn create_agent_chat_stamps_agent_via_grant() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let sessions = sessions(store.clone());
    let result = sessions
        .create_agent_chat("coding", Some("Coder".into()))
        .await
        .unwrap();
    assert!(!result.reused);
    assert_eq!(result.profile, "coding");
    let header = store.header(id_of(&result)).await.unwrap();
    assert_eq!(header.surface_mode, SurfaceMode::Agent);
    assert_eq!(header.grant.profile.as_deref(), Some("coding"));
}

#[tokio::test]
async fn create_agent_chat_rejects_non_agent_profile() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let sessions = ChatSessions::new(
        store,
        Executor::new(
            Arc::new(MockProvider::with_script("m", vec![])),
            Budget::default(),
        ),
        Arc::new(NoopRuntime),
    )
    .with_profile_resolver(Arc::new(|_| Ok(SessionGrant::default())));
    let err = sessions
        .create_agent_chat("chat-default", None)
        .await
        .unwrap_err();
    assert!(err.contains("not in the deployment"), "{err}");
    let err = sessions.create_agent_chat("   ", None).await.unwrap_err();
    assert!(err.contains("non-empty"), "{err}");
}

/// A deployment that lists `designer` in its `[chat] agent_profiles` gets
/// `create_agent("designer")` to succeed; the converse — a profile in the
/// default set but absent from the deployment's set — is refused. This is
/// the cross-PR consistency test that pins the bug fixed by
/// `agent_profiles.is_agent(profile)` replacing the hardcoded free function.
#[tokio::test]
async fn create_agent_chat_honours_deployment_agent_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let resolver: ProfileGrantResolver = Arc::new(|name: &str| {
        Ok(SessionGrant {
            profile: Some(name.to_owned()),
            ..SessionGrant::default()
        })
    });
    let deployment_set = AgentProfiles::new(["coding", "designer"]);
    let sessions = Arc::new(
        ChatSessions::new(
            store.clone(),
            Executor::new(
                Arc::new(MockProvider::with_script("m", vec![])),
                Budget::default(),
            ),
            Arc::new(NoopRuntime),
        )
        .with_profile_resolver(resolver)
        .with_agent_profiles(deployment_set),
    );
    sessions.install_self_handle();

    let result = sessions
        .create_agent_chat("designer", Some("D".into()))
        .await
        .unwrap();
    assert!(!result.reused);
    assert_eq!(result.profile, "designer");
    let header = store.header(id_of(&result)).await.unwrap();
    assert_eq!(header.surface_mode, SurfaceMode::Agent);

    let err = sessions.create_agent_chat("life", None).await.unwrap_err();
    assert!(err.contains("not in the deployment"), "{err}");
}

#[tokio::test]
async fn non_creator_profile_gets_no_spawner_even_with_handle() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let sessions = Arc::new(ChatSessions::new(
        store,
        Executor::new(
            Arc::new(MockProvider::with_script("m", vec![])),
            Budget::default(),
        ),
        Arc::new(NoopRuntime),
    ));
    sessions.install_self_handle();
    assert!(sessions.agent_spawner_for_profile(Some("coding")).is_none());
    assert!(
        sessions
            .agent_spawner_for_profile(Some("operator"))
            .is_some()
    );
    assert!(sessions.agent_spawner_for_profile(None).is_some());
}
