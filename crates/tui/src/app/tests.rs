use super::*;
use crate::format::relative_time;
use crossterm::event::{MouseButton, MouseEventKind};

fn test_app() -> App {
    App::new("http://127.0.0.1:4201".to_string(), ThemeRegistry::new())
}
fn conv(id: &str, title: &str) -> ConvHeader {
    ConvHeader {
        id: id.into(),
        title: Some(title.into()),
        created_at: String::new(),
        parent_conversation: None,
        spawned_by: None,
    }
}
fn child_conv(id: &str, title: &str, parent: &str) -> ConvHeader {
    ConvHeader {
        id: id.into(),
        title: Some(title.into()),
        created_at: String::new(),
        parent_conversation: Some(parent.into()),
        spawned_by: None,
    }
}
fn left_click(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::empty(),
    }
}
fn scroll_up(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: col,
        row,
        modifiers: KeyModifiers::empty(),
    }
}
fn scroll_down(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: col,
        row,
        modifiers: KeyModifiers::empty(),
    }
}
fn set_layout(app: &mut App, chat: Rect, input: Rect, session_browser: Rect) {
    app.layout.chat = chat;
    app.layout.input = input;
    app.layout.session_browser = session_browser;
}
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}
fn ctrl_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

#[test]
fn ctrl_c_quits() {
    let mut app = test_app();
    let effects = app.handle_key(ctrl_key(KeyCode::Char('c')));
    assert_eq!(effects.len(), 1);
    assert!(matches!(effects[0], Effect::Quit));
}
#[test]
fn enter_sends_message_when_input_has_text() {
    let mut app = test_app();
    app.input = "hello".to_string();
    app.cursor = 5;
    let effects = app.handle_key(key(KeyCode::Enter));
    assert!(app.streaming);
    assert!(app.input.is_empty());
    assert_eq!(effects.len(), 1);
    assert!(matches!(effects[0], Effect::StartChatStream { .. }));
}
#[test]
fn enter_does_nothing_when_input_empty() {
    let mut app = test_app();
    let effects = app.handle_key(key(KeyCode::Enter));
    assert!(!app.streaming);
    assert_eq!(effects.len(), 1);
    assert!(matches!(effects[0], Effect::None));
}
#[test]
fn enter_blocked_during_streaming() {
    let mut app = test_app();
    app.streaming = true;
    app.input = "hello".to_string();
    let effects = app.handle_key(key(KeyCode::Enter));
    assert_eq!(effects.len(), 1);
    assert!(matches!(effects[0], Effect::None));
}
#[test]
fn typing_inserts_character() {
    let mut app = test_app();
    app.handle_key(key(KeyCode::Char('a')));
    app.handle_key(key(KeyCode::Char('b')));
    assert_eq!(app.input, "ab");
}
#[test]
fn backspace_removes_character_before_cursor() {
    let mut app = test_app();
    app.input = "ab".to_string();
    app.cursor = 1;
    app.handle_key(key(KeyCode::Backspace));
    assert_eq!(app.input, "b");
    assert_eq!(app.cursor, 0);
}
#[test]
fn esc_clears_input() {
    let mut app = test_app();
    app.input = "hello".to_string();
    app.cursor = 5;
    app.handle_key(key(KeyCode::Esc));
    assert!(app.input.is_empty());
    assert_eq!(app.cursor, 0);
}
#[test]
fn tab_switches_focus_to_chat() {
    let mut app = test_app();
    assert_eq!(app.focus, Focus::Input);
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.focus, Focus::ChatMessages);
}
#[test]
fn tab_from_chat_returns_via_esc() {
    let mut app = test_app();
    app.focus = Focus::ChatMessages;
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.focus, Focus::Input);
}
#[test]
fn esc_from_sidebar_returns_to_input() {
    let mut app = test_app();
    app.focus = Focus::SessionBrowser;
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.focus, Focus::Input);
}
#[test]
fn sidebar_jk_navigation() {
    let mut app = test_app();
    app.conversations = vec![conv("1", "a"), conv("2", "b")];
    app.focus = Focus::SessionBrowser;
    app.handle_key(key(KeyCode::Char('j')));
    assert_eq!(app.sidebar_selection, 1);
    app.handle_key(key(KeyCode::Char('j')));
    assert_eq!(app.sidebar_selection, 1);
    app.handle_key(key(KeyCode::Char('k')));
    assert_eq!(app.sidebar_selection, 0);
}
#[test]
fn sse_token_accumulates_in_assistant_buf() {
    let mut app = test_app();
    app.update(Action::SseToken("Hello".into()));
    app.update(Action::SseToken(" ".into()));
    app.update(Action::SseToken("world".into()));
    assert_eq!(app.assistant_buf, "Hello world");
}
#[test]
fn sse_done_finalizes_assistant_message() {
    let mut app = test_app();
    app.assistant_buf = "answer".to_string();
    let effects = app.update(Action::SseDone);
    assert!(!app.streaming);
    assert!(app.assistant_buf.is_empty());
    assert!(matches!(app.messages.last(), Some(Message::Assistant(m)) if m == "answer"));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::RefreshConversations))
    );
}
#[test]
fn sse_done_without_content_or_tool_calls_adds_nothing() {
    let mut app = test_app();
    app.update(Action::SseDone);
    assert!(app.messages.is_empty());
}
#[test]
fn slash_quit() {
    let mut app = test_app();
    app.input = "/quit".to_string();
    let effects = app.handle_key(key(KeyCode::Enter));
    assert!(app.input.is_empty());
    assert_eq!(effects.len(), 1);
    assert!(matches!(effects[0], Effect::Quit));
}
#[test]
fn slash_exit() {
    let mut app = test_app();
    app.input = "/exit".to_string();
    let effects = app.handle_key(key(KeyCode::Enter));
    assert!(app.input.is_empty());
    assert_eq!(effects.len(), 1);
    assert!(matches!(effects[0], Effect::Quit));
}
#[test]
fn slash_new() {
    let mut app = test_app();
    app.session = Some("old-session".into());
    app.messages.push(Message::User("hi".into()));
    app.streaming = true;
    app.input = "/new".to_string();
    let effects = app.handle_key(key(KeyCode::Enter));
    assert!(app.session.is_none());
    assert!(app.messages.is_empty());
    assert!(!app.streaming);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::RefreshConversations))
    );
}
#[test]
fn slash_clear() {
    let mut app = test_app();
    app.messages.push(Message::User("hi".into()));
    app.messages.push(Message::Assistant("hey".into()));
    app.assistant_buf = "streaming...".into();
    app.input = "/clear".to_string();
    app.handle_key(key(KeyCode::Enter));
    assert!(app.messages.is_empty());
    assert!(app.assistant_buf.is_empty());
    assert_eq!(app.scroll_offset, 0);
}
#[test]
fn slash_clear_resets_chat_cursor() {
    let mut app = test_app();
    app.messages = vec![Message::User("a".into()), Message::Assistant("b".into())];
    app.chat_cursor = 5;
    app.expanded_messages.insert(0);
    app.input = "/clear".to_string();
    app.handle_key(key(KeyCode::Enter));
    assert!(app.messages.is_empty());
    assert_eq!(app.chat_cursor, 0);
    assert!(app.expanded_messages.is_empty());
}
#[test]
fn slash_help_shows_help_text() {
    let mut app = test_app();
    app.input = "/help".to_string();
    app.handle_key(key(KeyCode::Enter));
    let last = app.messages.last().unwrap();
    assert!(matches!(last, Message::System(_)));
}
#[test]
fn slash_unknown_command() {
    let mut app = test_app();
    app.input = "/bogus".to_string();
    app.handle_key(key(KeyCode::Enter));
    let last = app.messages.last().unwrap();
    assert!(matches!(last, Message::System(m) if m.contains("Unknown command")));
}
#[test]
fn slash_works_during_streaming() {
    let mut app = test_app();
    app.session = Some("sess".into());
    app.streaming = true;
    app.input = "/new".to_string();
    let effects = app.handle_key(key(KeyCode::Enter));
    assert!(app.session.is_none());
    assert!(!app.streaming);
    assert!(app.input.is_empty());
    assert!(effects.iter().any(|e| matches!(e, Effect::CancelStream)));
}
#[test]
fn pgup_scrolls_up() {
    let mut app = test_app();
    app.handle_key(key(KeyCode::PageUp));
    assert_eq!(app.scroll_offset, 10);
}
#[test]
fn pgdown_scrolls_down() {
    let mut app = test_app();
    app.scroll_offset = 20;
    app.handle_key(key(KeyCode::PageDown));
    assert_eq!(app.scroll_offset, 10);
}
#[test]
fn pgdown_does_not_go_below_zero() {
    let mut app = test_app();
    app.scroll_offset = 3;
    app.handle_key(key(KeyCode::PageDown));
    assert_eq!(app.scroll_offset, 0);
}
#[test]
fn history_loaded_renders_tool_calls() {
    let mut app = test_app();
    app.pending_load = Some("c1".into());
    app.update(Action::HistoryLoaded { id: "c1".into(), messages: vec![ChatMessage { role: "assistant".into(), content: String::new(), tool_calls: Some(serde_json::json!([{"function":{"name":"search","arguments":"{\"q\":\"test\"}"}}])), tool_call_id: None }] });
    assert_eq!(app.messages.len(), 1);
    assert!(matches!(app.messages[0], Message::ToolCall(_)));
}
#[test]
fn history_loaded_mixed_content_and_tools() {
    let mut app = test_app();
    app.pending_load = Some("c2".into());
    app.update(Action::HistoryLoaded {
        id: "c2".into(),
        messages: vec![
            ChatMessage {
                role: "user".into(),
                content: "search please".into(),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".into(),
                content: "Let me search...".into(),
                tool_calls: Some(
                    serde_json::json!([{"function":{"name":"search","arguments":"{}"}}]),
                ),
                tool_call_id: None,
            },
        ],
    });
    assert_eq!(app.messages.len(), 3);
    assert!(matches!(app.messages[0], Message::User(_)));
    assert!(matches!(app.messages[1], Message::ToolCall(_)));
    assert!(matches!(app.messages[2], Message::Assistant(_)));
}
#[test]
fn history_loaded_enforces_message_cap() {
    let mut app = test_app();
    app.pending_load = Some("big-conv".into());
    let many_messages: Vec<ChatMessage> = (0..600)
        .map(|i| ChatMessage {
            role: "user".into(),
            content: format!("message {i}"),
            tool_calls: None,
            tool_call_id: None,
        })
        .collect();
    app.update(Action::HistoryLoaded {
        id: "big-conv".into(),
        messages: many_messages,
    });
    // After pruning: 500 messages kept + 1 system marker = 501 total
    assert_eq!(app.messages.len(), 501);
    // First message is the system marker
    assert!(
        matches!(&app.messages[0], Message::System(s) if s == "... 100 earlier messages omitted")
    );
    // The remaining 500 should be the last 500 user messages (indices 100..600)
    assert!(matches!(&app.messages[1], Message::User(m) if m == "message 100"));
    assert!(matches!(&app.messages[500], Message::User(m) if m == "message 599"));
}
#[test]
fn sse_failed_pushes_system_message() {
    let mut app = test_app();
    app.update(Action::SseFailed("connection lost".into()));
    assert!(!app.streaming);
    assert!(app.assistant_buf.is_empty());
    assert!(
        matches!(app.messages.last(), Some(Message::System(m)) if m.contains("connection lost"))
    );
}
#[test]
fn sse_failed_clears_partial_assistant_buf() {
    let mut app = test_app();
    app.update(Action::SseToken("partial ".into()));
    app.update(Action::SseToken("response".into()));
    assert_eq!(app.assistant_buf, "partial response");
    app.update(Action::SseFailed("timeout".into()));
    assert!(app.assistant_buf.is_empty());
    assert!(!app.streaming);
    assert!(matches!(app.messages.last(), Some(Message::System(m)) if m.contains("timeout")));
}
#[test]
fn esc_during_streaming_cancels() {
    let mut app = test_app();
    app.streaming = true;
    app.assistant_buf = "partial response".into();
    let effects = app.handle_key(key(KeyCode::Esc));
    assert!(!app.streaming);
    assert!(app.assistant_buf.is_empty());
    assert!(matches!(app.messages.last(), Some(Message::System(m)) if m.contains("cancelled")));
    assert_eq!(effects.len(), 1);
    assert!(matches!(effects[0], Effect::CancelStream));
}
#[test]
fn ctrl_s_stops_streaming() {
    let mut app = test_app();
    app.streaming = true;
    app.assistant_buf = "partial".into();
    let effects = app.handle_key(ctrl_key(KeyCode::Char('s')));
    assert!(!app.streaming);
    assert!(app.assistant_buf.is_empty());
    assert!(matches!(app.messages.last(), Some(Message::System(m)) if m.contains("stopped")));
    assert!(matches!(effects[0], Effect::CancelStream));
}
#[test]
fn ctrl_s_without_streaming_does_nothing() {
    let mut app = test_app();
    let effects = app.handle_key(ctrl_key(KeyCode::Char('s')));
    assert!(!app.streaming);
    assert_eq!(effects.len(), 1);
    assert!(matches!(effects[0], Effect::None));
}
#[test]
fn esc_without_streaming_clears_input() {
    let mut app = test_app();
    app.input = "hello".into();
    app.cursor = 5;
    app.handle_key(key(KeyCode::Esc));
    assert!(app.input.is_empty());
    assert_eq!(app.cursor, 0);
}
#[test]
fn esc_without_streaming_empty_input_noop() {
    let mut app = test_app();
    let effects = app.handle_key(key(KeyCode::Esc));
    assert!(app.input.is_empty());
    assert_eq!(effects.len(), 1);
    assert!(matches!(effects[0], Effect::None));
}
#[test]
fn pending_load_set_on_sidebar_enter() {
    let mut app = test_app();
    app.conversations = vec![conv("c1", "test")];
    app.focus = Focus::SessionBrowser;
    let effects = app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.pending_load, Some("c1".into()));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::LoadConversationHistory(_)))
    );
}
#[test]
fn pending_load_cleared_on_history_loaded() {
    let mut app = test_app();
    app.pending_load = Some("c1".into());
    app.update(Action::HistoryLoaded {
        id: "c1".into(),
        messages: vec![],
    });
    assert!(app.pending_load.is_none());
}
#[test]
fn pending_load_cleared_on_sse_failed() {
    let mut app = test_app();
    app.pending_load = Some("c1".into());
    app.update(Action::SseFailed("error".into()));
    assert!(app.pending_load.is_none());
}
#[test]
fn relative_time_now() {
    let now = chrono::Utc::now().to_rfc3339();
    let rel = relative_time(&now);
    assert!(rel.ends_with("s ago") || rel == "0s ago");
}
#[test]
fn relative_time_past() {
    let past = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
    assert_eq!(relative_time(&past), "5m ago");
}
#[test]
fn relative_time_invalid_iso_returns_raw() {
    assert_eq!(relative_time("not-a-date"), "not-a-date");
}
#[test]
fn sidebar_filter_filters_conversations() {
    let mut app = test_app();
    app.conversations = vec![
        conv("1", "debug session"),
        conv("2", "deploy notes"),
        conv("3", "meeting"),
    ];
    app.sidebar_filter = "de".into();
    let filtered = app.filtered_conversations();
    assert_eq!(filtered.len(), 2);
}
#[test]
fn sidebar_filter_empty_returns_all() {
    let mut app = test_app();
    app.conversations = vec![conv("1", "a"), conv("2", "b")];
    let filtered = app.filtered_conversations();
    assert_eq!(filtered.len(), 2);
}
#[test]
fn typing_in_sidebar_appends_to_filter() {
    let mut app = test_app();
    app.focus = Focus::SessionBrowser;
    app.handle_key(key(KeyCode::Char('s')));
    app.handle_key(key(KeyCode::Char('e')));
    assert_eq!(app.sidebar_filter, "se");
}
#[test]
fn backspace_in_sidebar_removes_filter_char() {
    let mut app = test_app();
    app.focus = Focus::SessionBrowser;
    app.sidebar_filter = "ab".into();
    app.handle_key(key(KeyCode::Backspace));
    assert_eq!(app.sidebar_filter, "a");
}
#[test]
fn esc_clears_sidebar_filter_then_returns_to_input() {
    let mut app = test_app();
    app.focus = Focus::SessionBrowser;
    app.sidebar_filter = "search".into();
    app.handle_key(key(KeyCode::Esc));
    assert!(app.sidebar_filter.is_empty());
    assert_eq!(app.focus, Focus::Input);
}
#[test]
fn esc_from_session_browser_clears_filter() {
    let mut app = test_app();
    app.focus = Focus::SessionBrowser;
    app.sidebar_filter = "search".into();
    app.handle_key(key(KeyCode::Esc));
    assert!(app.sidebar_filter.is_empty());
    assert_eq!(app.focus, Focus::Input);
}
#[test]
fn n_with_filter_appends_not_new_conversation() {
    let mut app = test_app();
    app.focus = Focus::SessionBrowser;
    app.sidebar_filter = "pytho".into();
    app.session = Some("old".into());
    app.handle_key(key(KeyCode::Char('n')));
    assert_eq!(app.sidebar_filter, "python");
    assert!(app.session.is_some());
}
#[test]
fn slash_theme_dark() {
    let mut app = test_app();
    app.input = "/theme dark".into();
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.theme.name, "dark");
}
#[test]
fn slash_theme_light() {
    let mut app = test_app();
    app.theme = Theme::default_dark();
    app.input = "/theme light".into();
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.theme.name, "light");
}
#[test]
fn slash_theme_list() {
    let mut app = test_app();
    app.input = "/theme list".into();
    app.handle_key(key(KeyCode::Enter));
    let last = app.messages.last().unwrap();
    assert!(matches!(last, Message::System(m) if m.contains("Available themes")));
}
#[test]
fn slash_theme_unknown() {
    let mut app = test_app();
    app.input = "/theme bogus".into();
    app.handle_key(key(KeyCode::Enter));
    let last = app.messages.last().unwrap();
    assert!(matches!(last, Message::System(m) if m.contains("Usage")));
}
#[test]
fn slash_model_opens_browser() {
    let mut app = test_app();
    app.input = "/model".into();
    let effects = app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.focus, Focus::ModelBrowser);
    assert!(app.models_loading);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::FetchModels)),
        "expected FetchModels effect"
    );
    let last = app.messages.last().unwrap();
    assert!(matches!(last, Message::System(m) if m.contains("model") || m.contains("Not connected")));
}

#[test]
fn model_browser_filter_and_selection() {
    let mut app = test_app();
    app.models = vec![
        "deepseek-chat".into(),
        "deepseek-reasoner".into(),
        "gpt-4o".into(),
    ];
    app.open_model_browser();
    app.models_loading = false;
    app.sidebar_filter = "deep".into();
    let filtered = app.filtered_models();
    assert_eq!(filtered.len(), 2);
    assert!(filtered.iter().all(|m| m.contains("deepseek")));
    app.clamp_model_selection();
    assert_eq!(app.sidebar_selection, 0);
}

#[test]
fn models_loaded_action_fills_catalog() {
    let mut app = test_app();
    app.open_model_browser();
    assert!(app.models_loading);
    app.update(Action::ModelsLoaded {
        models: vec!["a".into(), "b".into()],
        error: None,
    });
    assert!(!app.models_loading);
    assert_eq!(app.models, vec!["a".to_string(), "b".to_string()]);
    assert!(app.models_error.is_none());
}
#[test]
fn slash_session_close() {
    let mut app = test_app();
    app.session = Some("sess-1".into());
    app.input = "/session close".into();
    app.handle_key(key(KeyCode::Enter));
    assert!(app.session.is_none());
    let last = app.messages.last().unwrap();
    assert!(matches!(last, Message::System(m) if m.contains("Closed")));
}
#[test]
fn slash_session_info() {
    let mut app = test_app();
    app.session = Some("c1".into());
    app.conversations = vec![conv("c1", "test conv")];
    app.messages.push(Message::User("hi".into()));
    app.input = "/session info".into();
    app.handle_key(key(KeyCode::Enter));
    let last = app.messages.last().unwrap();
    assert!(matches!(last, Message::System(m) if m.contains("c1") && m.contains("test conv")));
}
#[test]
fn slash_session_info_no_session() {
    let mut app = test_app();
    app.input = "/session info".into();
    app.handle_key(key(KeyCode::Enter));
    let last = app.messages.last().unwrap();
    assert!(matches!(last, Message::System(m) if m.contains("No active session")));
}
#[test]
fn slash_session_list() {
    let mut app = test_app();
    app.conversations = vec![conv("c1", "alpha"), conv("c2", "beta")];
    app.input = "/session list".into();
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.focus, Focus::SessionBrowser);
    assert!(app.input.is_empty());
}
#[test]
fn connection_status_flips_connected() {
    let mut app = test_app();
    app.daemon_connected = false;
    app.update(Action::ConnectionStatus(true));
    assert!(app.daemon_connected);
}
#[test]
fn connection_lost_adds_system_message() {
    let mut app = test_app();
    app.daemon_connected = true;
    app.update(Action::ConnectionStatus(false));
    assert!(!app.daemon_connected);
    assert!(
        app.messages.iter().any(
            |m| matches!(m, Message::System(s) if s.contains("Connection to daemon lost"))
        )
    );
}
#[test]
fn connection_restored_adds_reconnect_message() {
    let mut app = test_app();
    app.daemon_connected = false;
    app.update(Action::ConnectionStatus(true));
    assert!(app.daemon_connected);
    assert!(
        app.messages
            .iter()
            .any(|m| matches!(m, Message::System(s) if s.contains("Reconnected")))
    );
}
#[test]
fn connection_status_no_spurious_messages() {
    let mut app = test_app();
    app.daemon_connected = false;
    let before = app.messages.len();
    app.update(Action::ConnectionStatus(false));
    assert_eq!(app.messages.len(), before);
}

// â”€â”€ Tree tests â”€â”€

#[test]
fn visible_tree_flat_roots() {
    let mut app = test_app();
    app.conversations = vec![conv("1", "a"), conv("2", "b")];
    let visible = app.visible_conversations();
    assert_eq!(visible.len(), 2);
    assert_eq!(visible[0].depth, 0);
    assert_eq!(visible[1].depth, 0);
    assert!(!visible[0].has_children);
}

#[test]
fn visible_tree_children_indented() {
    let mut app = test_app();
    app.conversations = vec![conv("1", "parent"), child_conv("2", "child", "1")];
    let visible = app.visible_conversations();
    assert_eq!(visible.len(), 2);
    assert_eq!(visible[0].depth, 0);
    assert_eq!(visible[1].depth, 1);
    assert!(visible[1].is_last);
    assert!(visible[0].has_children);
}

#[test]
fn visible_tree_collapse_hides_children() {
    let mut app = test_app();
    app.conversations = vec![
        conv("1", "parent"),
        child_conv("2", "child", "1"),
        child_conv("3", "child2", "1"),
    ];
    app.collapsed_nodes.insert("1".into());
    let visible = app.visible_conversations();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].header.id, "1");
    assert!(visible[0].collapsed);
}

#[test]
fn visible_tree_expand_shows_children() {
    let mut app = test_app();
    app.conversations = vec![conv("1", "parent"), child_conv("2", "child", "1")];
    let visible = app.visible_conversations();
    assert_eq!(visible.len(), 2);
}

#[test]
fn sidebar_space_appends_to_filter() {
    let mut app = test_app();
    app.conversations = vec![conv("1", "parent"), child_conv("2", "child", "1")];
    app.focus = Focus::SessionBrowser;
    app.handle_key(key(KeyCode::Char(' ')));
    assert_eq!(app.sidebar_filter, " ");
    assert!(app.collapsed_nodes.is_empty());
}

#[test]
fn sidebar_enter_on_leaf_loads_conversation() {
    let mut app = test_app();
    app.conversations = vec![conv("c1", "leaf")];
    app.focus = Focus::SessionBrowser;
    let effects = app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.pending_load, Some("c1".into()));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::LoadConversationHistory(_)))
    );
}

#[test]
fn sidebar_enter_on_parent_opens_it() {
    let mut app = test_app();
    app.conversations = vec![conv("1", "parent"), child_conv("2", "child", "1")];
    app.focus = Focus::SessionBrowser;
    app.sidebar_selection = 0;
    let effects = app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.pending_load, Some("1".into()));
    assert_eq!(app.focus, Focus::Input);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::LoadConversationHistory(_)))
    );
}

#[test]
fn visible_tree_filter_matches_across_tree() {
    let mut app = test_app();
    app.conversations = vec![
        conv("1", "root"),
        child_conv("2", "branch", "1"),
        conv("3", "other"),
    ];
    app.sidebar_filter = "root".into();
    let visible = app.visible_conversations();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].header.id, "1");
}

#[test]
fn visible_tree_filter_returns_empty_on_mismatch() {
    let mut app = test_app();
    app.conversations = vec![conv("1", "alpha"), child_conv("2", "beta", "1")];
    app.sidebar_filter = "zzz".into();
    let visible = app.visible_conversations();
    assert!(visible.is_empty());
}

#[test]
fn slash_fork_with_session() {
    let mut app = test_app();
    app.session = Some("c1".into());
    app.input = "/fork".into();
    let effects = app.handle_key(key(KeyCode::Enter));
    let last = app.messages.last().unwrap();
    assert!(matches!(last, Message::System(m) if m.contains("Forking")));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::ForkConversation(_)))
    );
}

#[test]
fn slash_fork_no_session() {
    let mut app = test_app();
    app.input = "/fork".into();
    app.handle_key(key(KeyCode::Enter));
    let last = app.messages.last().unwrap();
    assert!(matches!(last, Message::System(m) if m.contains("No active session")));
}

// â”€â”€ Mouse tests â”€â”€

#[test]
fn mouse_click_chat_focuses_chat() {
    let mut app = test_app();
    app.focus = Focus::Input;
    app.messages.push(Message::System("hi".into()));
    set_layout(
        &mut app,
        Rect::new(0, 1, 80, 20),
        Rect::new(0, 21, 80, 3),
        Rect::default(),
    );
    app.handle_mouse(left_click(5, 5));
    assert_eq!(app.focus, Focus::ChatMessages);
}

#[test]
fn mouse_click_input_focuses_input() {
    let mut app = test_app();
    app.focus = Focus::ChatMessages;
    app.input = "hello".to_string();
    set_layout(
        &mut app,
        Rect::new(0, 1, 80, 20),
        Rect::new(0, 21, 80, 3),
        Rect::default(),
    );
    app.handle_mouse(left_click(2, 22));
    assert_eq!(app.focus, Focus::Input);
}

#[test]
fn mouse_click_session_browser_selects_item() {
    let mut app = test_app();
    app.conversations = vec![conv("c1", "test"), conv("c2", "other")];
    app.focus = Focus::SessionBrowser;
    // Full-screen browser: list starts at y=4 (3-row filter + 1).
    set_layout(
        &mut app,
        Rect::default(),
        Rect::default(),
        Rect::new(0, 0, 80, 30),
    );
    app.handle_mouse(left_click(2, 5)); // first list row
    assert_eq!(app.focus, Focus::SessionBrowser);
    assert_eq!(app.sidebar_selection, 1); // second item at y=5
}

#[test]
fn mouse_click_session_browser_double_opens() {
    let mut app = test_app();
    app.conversations = vec![conv("c1", "test")];
    app.focus = Focus::SessionBrowser;
    app.sidebar_selection = 0;
    set_layout(
        &mut app,
        Rect::default(),
        Rect::default(),
        Rect::new(0, 0, 80, 30),
    );
    // First click on same selection opens (idx == prev).
    let effects = app.handle_mouse(left_click(2, 4));
    assert_eq!(app.pending_load, Some("c1".into()));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::LoadConversationHistory(_)))
    );
}

#[test]
fn mouse_click_session_browser_second_item() {
    let mut app = test_app();
    app.conversations = vec![conv("1", "parent"), conv("2", "other")];
    app.focus = Focus::SessionBrowser;
    app.sidebar_selection = 0;
    set_layout(
        &mut app,
        Rect::default(),
        Rect::default(),
        Rect::new(0, 0, 80, 30),
    );
    app.handle_mouse(left_click(2, 5));
    assert_eq!(app.sidebar_selection, 1);
}

#[test]
fn mouse_scroll_chat() {
    let mut app = test_app();
    app.scroll_offset = 10;
    set_layout(
        &mut app,
        Rect::new(0, 0, 60, 20),
        Rect::new(0, 21, 60, 3),
        Rect::new(61, 0, 20, 20),
    );
    app.handle_mouse(scroll_up(5, 5));
    assert_eq!(app.scroll_offset, 7);
    app.handle_mouse(scroll_down(5, 5));
    assert_eq!(app.scroll_offset, 10);
}

#[test]
fn mouse_scroll_sidebar() {
    let mut app = test_app();
    app.conversations = vec![conv("c1", "a"), conv("c2", "b"), conv("c3", "c")];
    app.focus = Focus::SessionBrowser;
    app.sidebar_selection = 1;
    set_layout(
        &mut app,
        Rect::new(0, 0, 60, 20),
        Rect::new(0, 21, 60, 3),
        Rect::new(61, 0, 20, 20),
    );
    app.handle_mouse(scroll_up(62, 2));
    assert_eq!(app.sidebar_selection, 0);
    app.handle_mouse(scroll_down(62, 2));
    assert_eq!(app.sidebar_selection, 1);
}

#[test]
fn sidebar_enter_empty_conversations_does_not_panic() {
    let mut app = test_app();
    app.conversations = vec![];
    app.focus = Focus::SessionBrowser;
    let effects = app.handle_key(key(KeyCode::Enter));
    assert!(effects.iter().all(|e| matches!(e, Effect::None)));
}

#[test]
fn history_loaded_stale_response_rejected() {
    let mut app = test_app();
    app.pending_load = Some("newer".into());
    app.messages.push(Message::User("current".into()));
    let effects = app.update(Action::HistoryLoaded {
        id: "stale".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: "stale".into(),
            tool_calls: None,
            tool_call_id: None,
        }],
    });
    assert!(matches!(app.messages[0], Message::User(ref m) if m == "current"));
    assert!(effects.is_empty() || effects.iter().all(|e| matches!(e, Effect::None)));
}
#[test]
fn pending_load_cleared_on_disconnect() {
    let mut app = test_app();
    app.pending_load = Some("c1".into());
    app.update(Action::ConnectionStatus(false));
    assert!(app.pending_load.is_none());
}

#[test]
fn sidebar_selection_clamped_after_filter_cleared() {
    let mut app = test_app();
    app.conversations = vec![conv("c1", "alpha"), conv("c2", "beta"), conv("c3", "gamma")];
    app.sidebar_selection = 2;
    app.sidebar_filter = "gamma".into();
    app.focus = Focus::SessionBrowser;
    app.handle_key(key(KeyCode::Esc));
    assert!(app.sidebar_filter.is_empty());
    assert!(app.sidebar_selection < app.visible_conversations().len());
}

#[test]
fn new_conversation_clears_pending_load() {
    let mut app = test_app();
    app.pending_load = Some("c1".into());
    app.session = Some("old".into());
    app.input = "/new".into();
    app.handle_key(key(KeyCode::Enter));
    assert!(app.pending_load.is_none());
}

// â”€â”€ Cursor movement keys â”€â”€

#[test]
fn delete_removes_char_after_cursor() {
    let mut app = test_app();
    app.input = "abc".into();
    app.cursor = 1;
    app.handle_key(key(KeyCode::Delete));
    assert_eq!(app.input, "ac");
    assert_eq!(app.cursor, 1);
}
#[test]
fn delete_at_end_does_nothing() {
    let mut app = test_app();
    app.input = "abc".into();
    app.cursor = 3;
    app.handle_key(key(KeyCode::Delete));
    assert_eq!(app.input, "abc");
}
#[test]
fn left_moves_cursor() {
    let mut app = test_app();
    app.input = "abc".into();
    app.cursor = 2;
    app.handle_key(key(KeyCode::Left));
    assert_eq!(app.cursor, 1);
}
#[test]
fn left_at_zero_does_nothing() {
    let mut app = test_app();
    app.input = "abc".into();
    app.cursor = 0;
    app.handle_key(key(KeyCode::Left));
    assert_eq!(app.cursor, 0);
}
#[test]
fn right_moves_cursor() {
    let mut app = test_app();
    app.input = "abc".into();
    app.cursor = 1;
    app.handle_key(key(KeyCode::Right));
    assert_eq!(app.cursor, 2);
}
#[test]
fn right_at_end_does_nothing() {
    let mut app = test_app();
    app.input = "abc".into();
    app.cursor = 3;
    app.handle_key(key(KeyCode::Right));
    assert_eq!(app.cursor, 3);
}
#[test]
fn home_jumps_to_start() {
    let mut app = test_app();
    app.input = "abc".into();
    app.cursor = 3;
    app.handle_key(key(KeyCode::Home));
    assert_eq!(app.cursor, 0);
}
#[test]
fn end_jumps_to_end() {
    let mut app = test_app();
    app.input = "abc".into();
    app.cursor = 0;
    app.handle_key(key(KeyCode::End));
    assert_eq!(app.cursor, 3);
}

// â”€â”€ Shift+Enter and sidebar edge cases â”€â”€

#[test]
fn shift_enter_inserts_newline() {
    let mut app = test_app();
    app.input = "line1".into();
    app.cursor = 5;
    let mut key_event = key(KeyCode::Enter);
    key_event.modifiers.insert(KeyModifiers::SHIFT);
    app.handle_key(key_event);
    assert_eq!(app.input, "line1\n");
    assert_eq!(app.cursor, 6);
}
#[test]
fn space_on_leaf_node_appends_to_filter() {
    let mut app = test_app();
    app.conversations = vec![conv("c1", "leaf")];
    app.focus = Focus::SessionBrowser;
    app.handle_key(key(KeyCode::Char(' ')));
    assert_eq!(app.sidebar_filter, " ");
    assert_eq!(app.sidebar_selection, 0);
}
#[test]
fn ctrl_s_from_sidebar_stops_streaming() {
    let mut app = test_app();
    app.focus = Focus::SessionBrowser;
    app.streaming = true;
    app.assistant_buf = "partial".into();
    app.handle_key(ctrl_key(KeyCode::Char('s')));
    assert!(!app.streaming);
    assert!(app.assistant_buf.is_empty());
}
#[test]
fn ctrl_s_with_empty_buf_still_sends_stopped() {
    let mut app = test_app();
    app.streaming = true;
    app.assistant_buf.clear();
    app.handle_key(ctrl_key(KeyCode::Char('s')));
    assert!(!app.streaming);
    assert!(matches!(app.messages.last(), Some(Message::System(m)) if m.contains("stopped")));
}

// â”€â”€ Slash command edge cases â”€â”€

#[test]
fn slash_status_full() {
    let mut app = test_app();
    app.status = Some(DaemonStatus {
        running: true,
        vault_path: "/vault".into(),
        uptime_seconds: 120,
        watcher_active: true,
        dispatcher_attached: true,
        orchestrator_attached: false,
        reactions_seen: 7,
        model_name: Some("deepseek-chat".into()),
        token_usage_total: Some(500),
        context_window: Some(128000),
        chat_tools: 1,
        chat_tool_names: vec!["tasks:add".into()],
    });
    app.input = "/status".into();
    app.handle_key(key(KeyCode::Enter));
    let last = app.messages.last().unwrap();
    assert!(
        matches!(last, Message::System(m) if m.contains("attached") && m.contains("detached") && m.contains("deepseek-chat"))
    );
}
#[test]
fn slash_status_no_connection() {
    let mut app = test_app();
    app.status = None;
    app.input = "/status".into();
    app.handle_key(key(KeyCode::Enter));
    let last = app.messages.last().unwrap();
    assert!(matches!(last, Message::System(m) if m.contains("Not connected")));
}
#[test]
fn slash_theme_set_no_name() {
    let mut app = test_app();
    app.input = "/theme set".into();
    app.handle_key(key(KeyCode::Enter));
    let last = app.messages.last().unwrap();
    assert!(matches!(last, Message::System(m) if m.contains("Usage")));
}
#[test]
fn slash_session_switch_no_id() {
    let mut app = test_app();
    app.input = "/session switch".into();
    app.handle_key(key(KeyCode::Enter));
    let last = app.messages.last().unwrap();
    assert!(matches!(last, Message::System(m) if m.contains("Usage")));
}
#[test]
fn slash_session_switch_non_matching_id() {
    let mut app = test_app();
    app.conversations = vec![conv("abc123", "test")];
    app.input = "/session switch xyz".into();
    let effects = app.handle_key(key(KeyCode::Enter));
    // No conversation's id starts with "xyz" â€” falls back to using it verbatim (matches the
    // sidebar behavior: an unrecognized id still attempts a load rather than erroring).
    assert!(effects.iter().any(|e| matches!(e, Effect::LoadConversationHistory(id) if id == "xyz")));
    assert_eq!(app.pending_load, Some("xyz".to_string()));
}
#[test]
fn slash_session_close_no_session() {
    let mut app = test_app();
    app.session = None;
    app.input = "/session close".into();
    app.handle_key(key(KeyCode::Enter));
    let last = app.messages.last().unwrap();
    assert!(matches!(last, Message::System(m) if m.contains("No active session")));
}
#[test]
fn cmd_new_without_streaming_no_cancel() {
    let mut app = test_app();
    app.session = Some("sess".into());
    app.streaming = false;
    app.input = "/new".into();
    let effects = app.handle_key(key(KeyCode::Enter));
    assert!(!effects.iter().any(|e| matches!(e, Effect::CancelStream)));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::RefreshConversations))
    );
}

// â”€â”€ State machine edge cases â”€â”€

#[test]
fn conversations_update_clamps_selection() {
    let mut app = test_app();
    app.conversations = vec![
        conv("1", "a"),
        conv("2", "b"),
        conv("3", "c"),
        conv("4", "d"),
    ];
    app.sidebar_selection = 3;
    app.update(Action::ConversationsUpdate(vec![
        conv("1", "a"),
        conv("2", "b"),
    ]));
    assert_eq!(app.sidebar_selection, 1);
}
#[test]
fn sse_session_idempotent() {
    let mut app = test_app();
    app.update(Action::SseSession("first".into()));
    assert_eq!(app.session, Some("first".into()));
    app.update(Action::SseSession("second".into()));
    assert_eq!(app.session, Some("first".into()));
}
#[test]
fn push_tool_history_multiple_tools() {
    let mut app = test_app();
    app.push_tool_history(&serde_json::json!([{"function":{"name":"search","arguments":"{}"}},{"function":{"name":"read","arguments":"{\"path\":\"f\"}"}}]));
    assert_eq!(app.messages.len(), 2);
    assert!(matches!(app.messages[0], Message::ToolCall(ref c) if c.name == "search"));
}
#[test]
fn push_tool_history_non_array_is_noop() {
    let mut app = test_app();
    app.push_tool_history(&serde_json::json!({"not":"an array"}));
    assert!(app.messages.is_empty());
}
#[test]
fn scroll_back_saturating() {
    let mut app = test_app();
    app.scroll_offset = usize::MAX;
    app.scroll_back(10);
    assert_eq!(app.scroll_offset, usize::MAX);
}
#[test]
fn scroll_forward_saturating() {
    let mut app = test_app();
    app.scroll_offset = 0;
    app.scroll_forward(10);
    assert_eq!(app.scroll_offset, 0);
}

// â”€â”€ Tree: depth 2+ nesting â”€â”€

#[test]
fn visible_tree_depth_three() {
    let mut app = test_app();
    app.conversations = vec![
        conv("1", "root"),
        child_conv("2", "child", "1"),
        child_conv("3", "grandchild", "2"),
    ];
    let visible = app.visible_conversations();
    assert_eq!(visible.len(), 3);
    assert_eq!(visible[0].depth, 0);
    assert_eq!(visible[1].depth, 1);
    assert_eq!(visible[2].depth, 2);
    assert_eq!(visible[2].ancestors_last.len(), 2);
}

// â”€â”€ Utility functions â”€â”€
// format_uptime itself is tested in liberado-commands (the crate that now owns it) â€” see
// its format_uptime_hours_and_minutes/format_uptime_minutes_only/format_uptime_zero.

#[test]
fn truncate_for_display_exact_max() {
    assert_eq!(truncate_for_display("hello", 5), "hello");
}
#[test]
fn truncate_for_display_over_max() {
    assert_eq!(truncate_for_display("hello world", 8), "hello...");
}
#[test]
fn truncate_for_display_small_max() {
    assert_eq!(truncate_for_display("hello", 3), "...");
}
#[test]
fn mouse_scroll_sidebar_at_boundaries() {
    let mut app = test_app();
    app.conversations = vec![conv("c1", "a"), conv("c2", "b")];
    app.focus = Focus::SessionBrowser;
    set_layout(
        &mut app,
        Rect::new(0, 0, 60, 20),
        Rect::new(0, 21, 60, 3),
        Rect::new(61, 0, 20, 20),
    );
    app.sidebar_selection = 0;
    app.handle_mouse(scroll_up(62, 2));
    assert_eq!(app.sidebar_selection, 0);
    app.sidebar_selection = 1;
    app.handle_mouse(scroll_down(62, 2));
    assert_eq!(app.sidebar_selection, 1);
}
#[test]
fn mouse_click_input_sets_cursor_position() {
    let mut app = test_app();
    app.input = "hello".into();
    set_layout(
        &mut app,
        Rect::new(0, 0, 60, 20),
        Rect::new(0, 21, 60, 3),
        Rect::new(61, 0, 20, 20),
    );
    app.handle_mouse(left_click(4, 22));
    assert_eq!(app.focus, Focus::Input);
    assert_eq!(app.cursor, 3);
}
#[test]
fn session_browser_accepts_punctuation_in_filter() {
    let mut app = test_app();
    app.focus = Focus::SessionBrowser;
    app.sidebar_filter.clear();
    app.handle_key(key(KeyCode::Char('.')));
    app.handle_key(key(KeyCode::Char('@')));
    assert_eq!(app.sidebar_filter, ".@");
}
#[test]
fn sidebar_up_at_zero_does_nothing() {
    let mut app = test_app();
    app.conversations = vec![conv("c1", "a")];
    app.focus = Focus::SessionBrowser;
    app.sidebar_selection = 0;
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.sidebar_selection, 0);
}
#[test]
fn visible_tree_empty_input() {
    let mut app = test_app();
    app.conversations = vec![];
    let visible = app.visible_conversations();
    assert!(visible.is_empty());
}

// â”€â”€ Chat focus tests â”€â”€

#[test]
fn tab_cycles_input_and_chat() {
    let mut app = test_app();
    assert_eq!(app.focus, Focus::Input);
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.focus, Focus::ChatMessages);
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.focus, Focus::Input);
}

#[test]
fn slash_session_opens_browser() {
    let mut app = test_app();
    app.conversations = vec![conv("c1", "alpha")];
    app.input = "/session".into();
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.focus, Focus::SessionBrowser);
}

#[test]
fn chat_jk_navigates_messages() {
    let mut app = test_app();
    app.messages = vec![
        Message::User("a".into()),
        Message::Assistant("b".into()),
        Message::System("c".into()),
    ];
    app.focus = Focus::ChatMessages;
    app.chat_cursor = 0;
    app.handle_key(key(KeyCode::Char('j')));
    assert_eq!(app.chat_cursor, 1);
    app.handle_key(key(KeyCode::Char('k')));
    assert_eq!(app.chat_cursor, 0);
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.chat_cursor, 0);
    app.chat_cursor = 2;
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.chat_cursor, 2);
}

#[test]
fn chat_enter_toggles_expand() {
    let mut app = test_app();
    app.messages = vec![Message::ToolCall(ToolCallChip {
        name: "search".into(),
        args: "{}".into(),
    })];
    app.focus = Focus::ChatMessages;
    app.chat_cursor = 0;
    app.handle_key(key(KeyCode::Enter));
    assert!(app.expanded_messages.contains(&0));
    app.handle_key(key(KeyCode::Enter));
    assert!(!app.expanded_messages.contains(&0));
}

#[test]
fn chat_esc_returns_to_input() {
    let mut app = test_app();
    app.focus = Focus::ChatMessages;
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.focus, Focus::Input);
}

#[test]
fn chat_enter_out_of_bounds_noop() {
    let mut app = test_app();
    app.focus = Focus::ChatMessages;
    app.chat_cursor = 0;
    app.messages.clear();
    app.handle_key(key(KeyCode::Enter));
    assert!(app.expanded_messages.is_empty());
}

// â”€â”€ Word navigation â”€â”€

#[test]
fn prev_word_boundary_from_middle() {
    assert_eq!(prev_word_boundary("hello world rust", 10), 6);
}
#[test]
fn prev_word_boundary_from_start() {
    assert_eq!(prev_word_boundary("hello", 0), 0);
}
#[test]
fn prev_word_boundary_at_space() {
    assert_eq!(prev_word_boundary("hello world", 6), 0);
}
#[test]
fn prev_word_boundary_from_space() {
    assert_eq!(prev_word_boundary("  hello", 2), 0);
}
#[test]
fn next_word_boundary_from_middle() {
    assert_eq!(next_word_boundary("hello world rust", 6), 11);
}
#[test]
fn next_word_boundary_from_end() {
    assert_eq!(next_word_boundary("hello", 5), 5);
}
#[test]
fn next_word_boundary_to_word_start() {
    assert_eq!(next_word_boundary("hello world", 0), 5);
}

#[test]
fn ctrl_backspace_deletes_word() {
    let mut app = test_app();
    app.input = "hello world".into();
    app.cursor = 11;
    app.handle_key(ctrl_key(KeyCode::Backspace));
    assert_eq!(app.input, "hello ");
    assert_eq!(app.cursor, 6);
}

#[test]
fn ctrl_delete_deletes_next_word() {
    let mut app = test_app();
    app.input = "hello world rust".into();
    app.cursor = 6;
    app.handle_key(ctrl_key(KeyCode::Delete));
    assert_eq!(app.input, "hello rust");
}

#[test]
fn ctrl_left_moves_to_prev_word() {
    let mut app = test_app();
    app.input = "hello world".into();
    app.cursor = 10;
    app.handle_key(ctrl_key(KeyCode::Left));
    assert_eq!(app.cursor, 6);
}

#[test]
fn ctrl_right_moves_to_next_word() {
    let mut app = test_app();
    app.input = "hello world rust".into();
    app.cursor = 0;
    app.handle_key(ctrl_key(KeyCode::Right));
    assert_eq!(app.cursor, 5);
}

#[test]
fn ctrl_left_at_word_start_goes_to_prev_word() {
    let mut app = test_app();
    app.input = "a b c".into();
    app.cursor = 4;
    app.handle_key(ctrl_key(KeyCode::Left));
    assert_eq!(app.cursor, 2);
}

#[test]
fn ctrl_backspace_at_start_does_nothing() {
    let mut app = test_app();
    app.input = "hello".into();
    app.cursor = 0;
    app.handle_key(ctrl_key(KeyCode::Backspace));
    assert_eq!(app.input, "hello");
}

// â”€â”€ Coverage gap tests â”€â”€

#[test]
fn short_id_empty() {
    assert_eq!(crate::format::short_id(""), "");
}
#[test]
fn short_id_shorter_than_8() {
    assert_eq!(crate::format::short_id("abc"), "abc");
}
#[test]
fn short_id_exactly_8() {
    assert_eq!(crate::format::short_id("12345678"), "12345678");
}
#[test]
fn short_id_longer_than_8() {
    assert_eq!(crate::format::short_id("1234567890"), "12345678");
}

#[test]
fn action_tick_is_noop() {
    let mut app = test_app();
    let effects = app.update(Action::Tick);
    assert!(effects.iter().all(|e| matches!(e, Effect::None)));
}

#[test]
fn new_app_is_dirty_for_first_paint() {
    let app = test_app();
    assert!(app.is_dirty());
    assert!(app.should_draw());
}

#[test]
fn clear_dirty_skips_draw_when_idle_and_connected() {
    let mut app = test_app();
    app.daemon_connected = true;
    app.clear_dirty();
    assert!(!app.is_dirty());
    assert!(!app.needs_animation());
    assert!(!app.should_draw());
}

#[test]
fn tick_does_not_mark_dirty() {
    let mut app = test_app();
    app.daemon_connected = true;
    app.clear_dirty();
    app.update(Action::Tick);
    assert!(!app.is_dirty());
}

#[test]
fn identical_status_update_does_not_dirty() {
    let mut app = test_app();
    app.daemon_connected = true;
    let status = DaemonStatus {
        running: true,
        vault_path: "/v".into(),
        uptime_seconds: 10,
        watcher_active: false,
        dispatcher_attached: false,
        orchestrator_attached: false,
        reactions_seen: 0,
        model_name: None,
        token_usage_total: None,
        context_window: None,
        chat_tools: 0,
        chat_tool_names: vec![],
    };
    app.update(Action::StatusUpdate(status.clone()));
    app.clear_dirty();
    app.update(Action::StatusUpdate(status));
    assert!(!app.is_dirty());
}

#[test]
fn sse_token_marks_dirty() {
    let mut app = test_app();
    app.daemon_connected = true;
    app.clear_dirty();
    app.update(Action::SseToken("hi".into()));
    assert!(app.is_dirty());
    assert!(app.should_draw());
}

#[test]
fn streaming_forces_animation_draw_even_if_clean() {
    let mut app = test_app();
    app.daemon_connected = true;
    app.streaming = true;
    app.clear_dirty();
    assert!(app.needs_animation());
    assert!(app.should_draw());
}

#[test]
fn slash_palette_opens_on_slash() {
    let mut app = test_app();
    app.handle_key(key(KeyCode::Char('/')));
    assert!(!app.slash_matches().is_empty());
}

#[test]
fn slash_palette_narrows_and_tab_completes() {
    let mut app = test_app();
    for c in "/hel".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(app.slash_matches().len(), 1);
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.input, "/help");
}

#[test]
fn slash_ghost_suffix_shows_remainder() {
    let mut app = test_app();
    for c in "/hel".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(app.slash_ghost_suffix().as_deref(), Some("p"));
}

#[test]
fn slash_enter_accepts_ghost_without_tab() {
    let mut app = test_app();
    for c in "/hel".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    // Enter runs selected match (/help) without Tab materializing it first.
    app.handle_key(key(KeyCode::Enter));
    let has_help = app.messages.iter().any(|m| {
        matches!(m, Message::System(text) if text.contains("Slash commands"))
    });
    assert!(has_help, "expected /help catalog output, messages={:?}", app.messages);
    // Input cleared by the help handler after accept.
    assert!(app.input.is_empty());
}

#[test]
fn slash_palette_up_down_selects() {
    let mut app = test_app();
    app.handle_key(key(KeyCode::Char('/')));
    let n = app.slash_matches().len();
    assert!(n > 2);
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.slash_palette_index, 2);
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.slash_palette_index, 1);
}

#[test]
fn scroll_to_chat_cursor_noop_when_visible() {
    let mut app = test_app();
    app.messages = vec![Message::System("a".into()); 30];
    app.chat_cursor = 5;
    app.scroll_offset = 0;
    app.scroll_to_chat_cursor();
    assert_eq!(app.scroll_offset, 0);
}
#[test]
fn scroll_to_chat_cursor_scrolls_up() {
    let mut app = test_app();
    app.messages = vec![Message::System("a".into()); 30];
    app.chat_cursor = 3;
    app.scroll_offset = 10;
    app.scroll_to_chat_cursor();
    assert_eq!(app.scroll_offset, 3);
}
#[test]
fn scroll_to_chat_cursor_scrolls_down() {
    let mut app = test_app();
    app.messages = vec![Message::System("a".into()); 30];
    app.chat_cursor = 25;
    app.scroll_offset = 0;
    app.scroll_to_chat_cursor();
    assert_eq!(app.scroll_offset, 6);
}

#[test]
fn relative_time_exactly_60s() {
    let ts = chrono::Utc::now().to_rfc3339();
    assert!(!crate::format::relative_time(&ts).contains("m ago"));
}
#[test]
fn relative_time_future_returns_raw() {
    let fut = "2099-01-01T00:00:00Z";
    assert_eq!(crate::format::relative_time(fut), fut);
}

#[test]
fn push_tool_history_missing_function_field() {
    let mut app = test_app();
    app.push_tool_history(&serde_json::json!([{"other": "field"}]));
    assert_eq!(app.messages.len(), 1);
    assert!(matches!(app.messages[0], Message::ToolCall(ref c) if c.name == "?"));
}
#[test]
fn push_tool_history_non_string_args() {
    let mut app = test_app();
    app.push_tool_history(
        &serde_json::json!([{"function": {"name": "f", "arguments": {"key": "val"}}}]),
    );
    assert_eq!(app.messages.len(), 1);
    assert!(matches!(app.messages[0], Message::ToolCall(ref c) if c.args.contains("key")));
}
#[test]
fn push_tool_history_empty_array() {
    let mut app = test_app();
    app.push_tool_history(&serde_json::json!([]));
    assert!(app.messages.is_empty());
}

#[test]
fn sidebar_n_without_filter_creates_new() {
    let mut app = test_app();
    app.focus = Focus::SessionBrowser;
    app.sidebar_filter.clear();
    app.session = Some("old".into());
    app.messages.push(Message::User("hi".into()));
    app.handle_key(key(KeyCode::Char('n')));
    assert!(app.session.is_none());
    assert_eq!(app.focus, Focus::Input);
    // System notice about new conversation.
    assert!(app.messages.iter().any(|m| matches!(m, Message::System(_))));
    assert!(!app.messages.iter().any(|m| matches!(m, Message::User(_))));
}

#[test]
fn mouse_click_outside_all_panes() {
    let mut app = test_app();
    app.messages.push(Message::System("hi".into()));
    set_layout(
        &mut app,
        Rect::new(0, 0, 60, 20),
        Rect::new(0, 21, 60, 3),
        Rect::new(61, 0, 20, 20),
    );
    let effects = app.handle_mouse(left_click(99, 99));
    assert!(effects.iter().all(|e| matches!(e, Effect::None)));
}

#[test]
fn system_msg_pushes_and_resets_scroll() {
    let mut app = test_app();
    app.scroll_offset = 10;
    app.system_msg(String::from("test"), Effect::None);
    let last = app.messages.last().unwrap();
    assert!(matches!(last, Message::System(m) if m == "test"));
    assert_eq!(app.scroll_offset, 0);
}

#[test]
fn status_summary_model_name_none() {
    let mut app = test_app();
    app.status = Some(DaemonStatus {
        running: true,
        vault_path: "/v".into(),
        uptime_seconds: 0,
        watcher_active: false,
        dispatcher_attached: false,
        orchestrator_attached: false,
        reactions_seen: 0,
        model_name: None,
        token_usage_total: None,
        context_window: None,
        chat_tools: 0,
        chat_tool_names: Vec::new(),
    });
    let summary = app.status_summary();
    assert!(summary.model_name.is_none());
    assert_eq!(summary.message_count, 0);
}

#[test]
fn slash_status_context_window_zero() {
    let mut app = test_app();
    app.status = Some(DaemonStatus {
        running: true,
        vault_path: "/v".into(),
        uptime_seconds: 0,
        watcher_active: false,
        dispatcher_attached: false,
        orchestrator_attached: false,
        reactions_seen: 0,
        model_name: Some("m".into()),
        token_usage_total: Some(10),
        context_window: Some(0),
        chat_tools: 0,
        chat_tool_names: Vec::new(),
    });
    app.input = "/status".into();
    app.handle_key(key(KeyCode::Enter));
    let last = app.messages.last().unwrap();
    assert!(matches!(last, Message::System(m) if m.contains("--")));
}

#[test]
fn slash_theme_set_direct() {
    let mut app = test_app();
    app.input = "/theme dark".into();
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.theme.name, "dark");
}

#[test]
fn slash_theme_set_named() {
    let mut app = test_app();
    app.input = "/theme set dark".into();
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.theme.name, "dark");
}

#[test]
fn slash_session_bare_opens_browser() {
    let mut app = test_app();
    app.input = "/session".into();
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.focus, Focus::SessionBrowser);
}

#[test]
fn slash_session_unknown_subcommand() {
    let mut app = test_app();
    app.input = "/session foo".into();
    app.handle_key(key(KeyCode::Enter));
    let last = app.messages.last().unwrap();
    assert!(matches!(last, Message::System(m) if m.contains("Unknown session command")));
}

#[test]
fn slash_session_switch_success() {
    let mut app = test_app();
    app.conversations = vec![conv("abc123xx", "test")];
    app.input = "/session switch abc".into();
    let effects = app.handle_key(key(KeyCode::Enter));
    // "abc" is a prefix of "abc123xx" â€” resolves to the full id, matching the sidebar's
    // "type the first few characters" convention.
    assert!(effects.iter().any(|e| matches!(e, Effect::LoadConversationHistory(id) if id == "abc123xx")));
    assert_eq!(app.pending_load, Some("abc123xx".to_string()));
}

#[test]
fn prev_word_boundary_consecutive_spaces() {
    assert_eq!(prev_word_boundary("hello   world", 10), 8);
}

#[test]
fn visible_tree_filter_with_collapsed() {
    let mut app = test_app();
    app.conversations = vec![conv("1", "parent"), child_conv("2", "child", "1")];
    app.collapsed_nodes.insert("1".into());
    app.sidebar_filter = "child".into();
    let visible = app.visible_conversations();
    assert!(visible.is_empty());
}

#[test]
fn mouse_scroll_in_input_area_noop() {
    let mut app = test_app();
    set_layout(
        &mut app,
        Rect::new(0, 0, 60, 20),
        Rect::new(0, 21, 60, 3),
        Rect::new(61, 0, 20, 20),
    );
    app.handle_mouse(scroll_down(2, 22));
    assert_eq!(app.scroll_offset, 0);
}

fn set_content_width(app: &mut App, width: usize) {
    app.layout.input_content_width = width;
}

#[test]
fn cursor_visual_line_start() {
    let mut app = test_app();
    set_content_width(&mut app, 10);
    app.input = "hello world".to_string();
    app.cursor = 0;
    assert_eq!(app.cursor_visual_line(), 0);
}

#[test]
fn cursor_visual_line_within_line() {
    let mut app = test_app();
    set_content_width(&mut app, 10);
    app.input = "hello world".to_string();
    app.cursor = 7;
    assert_eq!(app.cursor_visual_line(), 0);
}

#[test]
fn cursor_visual_line_wraps() {
    let mut app = test_app();
    set_content_width(&mut app, 5);
    app.input = "hello world".to_string();
    app.cursor = 7;
    assert_eq!(app.cursor_visual_line(), 1);
}

#[test]
fn cursor_visual_line_newlines() {
    let mut app = test_app();
    set_content_width(&mut app, 10);
    app.input = "abc\ndef\nghi".to_string();
    app.cursor = 5;
    assert_eq!(app.cursor_visual_line(), 1);
}

#[test]
fn input_visual_lines_empty() {
    let mut app = test_app();
    set_content_width(&mut app, 10);
    assert_eq!(app.input_visual_lines(), 1);
}

#[test]
fn input_visual_lines_wraps() {
    let mut app = test_app();
    set_content_width(&mut app, 5);
    app.input = "hello world".to_string();
    assert_eq!(app.input_visual_lines(), 3);
}

#[test]
fn cursor_visual_col_start() {
    let mut app = test_app();
    set_content_width(&mut app, 10);
    app.input = "hello world".to_string();
    app.cursor = 0;
    assert_eq!(app.cursor_visual_col(), 0);
}

#[test]
fn cursor_visual_col_wraps() {
    let mut app = test_app();
    set_content_width(&mut app, 5);
    app.input = "hello world".to_string();
    app.cursor = 7;
    assert_eq!(app.cursor_visual_col(), 2);
}

#[test]
fn byte_offset_for_visual_start() {
    let mut app = test_app();
    set_content_width(&mut app, 10);
    app.input = "hello world".to_string();
    assert_eq!(app.byte_offset_for_visual(0, 0), 0);
}

#[test]
fn byte_offset_for_visual_mid_line() {
    let mut app = test_app();
    set_content_width(&mut app, 10);
    app.input = "hello world".to_string();
    assert_eq!(app.byte_offset_for_visual(0, 6), 6);
}

#[test]
fn byte_offset_for_visual_wrapped_line() {
    let mut app = test_app();
    set_content_width(&mut app, 5);
    app.input = "hello world".to_string();
    assert_eq!(app.byte_offset_for_visual(1, 0), 5);
}

#[test]
fn byte_offset_for_visual_past_end_clamps() {
    let mut app = test_app();
    set_content_width(&mut app, 10);
    app.input = "hi".to_string();
    assert_eq!(app.byte_offset_for_visual(0, 10), 2);
}

#[test]
fn handle_up_on_first_line_noop() {
    let mut app = test_app();
    set_content_width(&mut app, 10);
    app.input = "hello".to_string();
    app.cursor = 2;
    let effects = app.handle_key(key(KeyCode::Up));
    assert_eq!(app.cursor, 2);
    assert!(matches!(effects.as_slice(), [Effect::None]));
}

#[test]
fn handle_up_moves_one_line() {
    let mut app = test_app();
    set_content_width(&mut app, 5);
    app.input = "hello world".to_string();
    app.cursor = 7;
    let effects = app.handle_key(key(KeyCode::Up));
    assert_eq!(effects.len(), 1);
    assert!(matches!(effects[0], Effect::None));
    let moved_line = app.cursor_visual_line();
    assert_eq!(moved_line, 0);
}

#[test]
fn handle_down_on_last_line_noop() {
    let mut app = test_app();
    set_content_width(&mut app, 5);
    app.input = "hello world".to_string();
    app.cursor = 10;
    let effects = app.handle_key(key(KeyCode::Down));
    assert_eq!(app.cursor, 10);
    assert!(matches!(effects.as_slice(), [Effect::None]));
}

#[test]
fn handle_down_moves_one_line() {
    let mut app = test_app();
    set_content_width(&mut app, 5);
    app.input = "hello world".to_string();
    app.cursor = 2;
    let effects = app.handle_key(key(KeyCode::Down));
    assert_eq!(effects.len(), 1);
    assert!(matches!(effects[0], Effect::None));
    assert!(app.cursor_visual_line() >= 1);
}

#[test]
fn handle_up_roundtrip() {
    let mut app = test_app();
    set_content_width(&mut app, 3);
    app.input = "abcdefghij".to_string();
    app.cursor = 7;
    let original_col = app.cursor_visual_col();
    app.handle_key(key(KeyCode::Up));
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.cursor_visual_col(), original_col);
}
