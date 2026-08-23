//! Split from `telegram.rs` for module-health boundaries.

use super::*;

fn header(
    id: Ulid,
    title: &str,
    parent: Option<Ulid>,
) -> liberado_conversation_store::ConversationHeader {
    use liberado_conversation_store as lcs;
    lcs::ConversationHeader {
        id,
        title: if title.is_empty() {
            None
        } else {
            Some(title.into())
        },
        parent_conversation: parent,
        spawned_by: None,
        created_at: chrono::Utc::now(),
        grant: Default::default(),
    }
}

fn ctx_with(conversations: Vec<ConversationHeader>) -> TelegramCommandContext {
    TelegramCommandContext {
        session_id: Some("active-1".into()),
        messages: vec!["m1".into(), "m2".into()],
        conversations,
        goals_summary: vec![("goal-1".into(), "build the thing".into())],
        status: None,
        message_count: 7,
    }
}

/// Every CommandContext accessor answers from the real snapshot fields — stubs here would
/// feed the Telegram slash-command surface invented sessions, counts and titles.
#[tokio::test]
async fn command_context_accessors_answer_from_their_fields() {
    let a = Ulid::new();
    let b = Ulid::new();
    let mut ctx = ctx_with(vec![header(a, "First", None), header(b, "", Some(a))]);
    ctx.status = Some(StatusInfo {
        running: true,
        vault_path: "/v".into(),
        uptime_seconds: 9,
        model_name: Some("live-model".into()),
        token_usage_total: None,
        context_window: None,
        dispatcher_attached: false,
        orchestrator_attached: false,
        reactions_seen: 0,
    });

    assert!(!ctx.is_streaming());
    assert_eq!(ctx.conversation_count(), 2);
    assert_eq!(ctx.message_count, 7);
    assert_eq!(ctx.active_session_id(), Some("active-1"));

    // Prefix lookup: full id works, a short prefix resolves, a miss is None.
    let full = a.to_string();
    assert_eq!(
        ctx.find_conversation_id_by_prefix(&full).as_deref(),
        Some(full.as_str())
    );
    let short = &full[..8];
    assert_eq!(
        ctx.find_conversation_id_by_prefix(short).as_deref(),
        Some(full.as_str())
    );
    assert_eq!(ctx.find_conversation_id_by_prefix("zzzz"), None);

    // Status passes through.
    let status = ctx.status_info().expect("status present");
    assert_eq!(status.model_name.as_deref(), Some("live-model"));

    // Telegram has no theme engine: empty list, fixed label, reload refuses.
    assert!(ctx.theme_names().is_empty());
    assert_eq!(ctx.current_theme_name(), "n/a");
    assert!(!ctx.set_theme("dark"));
    assert!(ctx.reload_themes().is_err());

    // Titles resolve for known ids; blank titles are not invented.
    assert_eq!(ctx.conversation_title_for(&full).as_deref(), Some("First"));
    assert_eq!(ctx.conversation_title_for(&b.to_string()), None);
    assert_eq!(ctx.conversation_title_for("unknown"), None);

    // Parents resolve as strings; root conversations have none.
    assert_eq!(
        ctx.conversation_parent_for(&b.to_string()).as_deref(),
        Some(full.as_str())
    );
    assert_eq!(ctx.conversation_parent_for(&full), None);

    // The listing pairs titles with ids, marks untitled ones, appends goals last.
    let list = ctx.conversation_list();
    assert_eq!(
        list,
        vec![
            ("First".to_string(), full.clone()),
            ("(untitled)".to_string(), b.to_string()),
            ("[goal] build the thing".to_string(), "goal-1".to_string()),
        ]
    );
}

/// Mutators actually mutate: set/clear/reset are observable through the accessors.
#[tokio::test]
async fn command_context_mutators_take_effect() {
    let mut ctx = ctx_with(Vec::new());

    ctx.set_active_session(Some("sess-9".into()));
    assert_eq!(ctx.active_session_id(), Some("sess-9"));
    ctx.set_active_session(None);
    assert_eq!(ctx.active_session_id(), None);

    // Reset clears the active session (the messages vector has no trait-level reader;
    // its clearing is covered by reset's session half plus push_system_message users).
    ctx.set_active_session(Some("again".into()));
    ctx.reset_for_new_conversation();
    assert_eq!(ctx.active_session_id(), None);
    ctx.push_system_message("hello".into()); // must compile-and-run without state visible
}
