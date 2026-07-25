#[cfg(test)]
#[allow(clippy::module_inception)] // file is already `mod tests`; the inner wrapper is historical
mod tests {
    use crate::commands::*;
    use crate::context::{CommandContext, StatusInfo};
    use crate::dispatch::parse;
    use crate::result::CommandResult;
    use std::collections::HashMap;

    struct MockContext {
        pub session: Option<String>,
        pub streaming: bool,
        pub conversations: Vec<(String, String, Option<String>)>, // (id, title, parent)
        pub status: Option<StatusInfo>,
        pub theme_names_vec: Vec<String>,
        pub current_theme: String,
        pub messages: Vec<String>,
        pub input: String,
        pub theme_set_results: HashMap<String, bool>,
    }

    impl MockContext {
        fn new() -> Self {
            Self {
                session: None,
                streaming: false,
                conversations: Vec::new(),
                status: None,
                theme_names_vec: vec!["dark".into(), "light".into()],
                current_theme: "dark".into(),
                messages: Vec::new(),
                input: String::new(),
                theme_set_results: HashMap::new(),
            }
        }
    }

    impl CommandContext for MockContext {
        fn active_session_id(&self) -> Option<&str> {
            self.session.as_deref()
        }
        fn is_streaming(&self) -> bool {
            self.streaming
        }
        fn conversation_count(&self) -> usize {
            self.conversations.len()
        }
        fn find_conversation_id_by_prefix(&self, prefix: &str) -> Option<String> {
            self.conversations
                .iter()
                .find(|(id, _, _)| id.starts_with(prefix))
                .map(|(id, _, _)| id.clone())
        }
        fn status_info(&self) -> Option<StatusInfo> {
            self.status.clone()
        }
        fn theme_names(&self) -> Vec<String> {
            self.theme_names_vec.clone()
        }
        fn current_theme_name(&self) -> &str {
            &self.current_theme
        }
        fn conversation_title_for(&self, id: &str) -> Option<String> {
            self.conversations
                .iter()
                .find(|(cid, _, _)| cid == id)
                .map(|(_, title, _)| title.clone())
        }
        fn conversation_parent_for(&self, id: &str) -> Option<String> {
            self.conversations
                .iter()
                .find(|(cid, _, _)| cid == id)
                .and_then(|(_, _, parent)| parent.clone())
        }
        fn message_count(&self) -> usize {
            self.messages.len()
        }
        fn conversation_list(&self) -> Vec<(String, String)> {
            self.conversations
                .iter()
                .map(|(id, title, _)| (title.clone(), id.clone()))
                .collect()
        }

        fn set_active_session(&mut self, id: Option<String>) {
            self.session = id;
        }
        fn clear_chat(&mut self) {
            self.messages.clear();
        }
        fn reset_for_new_conversation(&mut self) {
            self.session = None;
            self.messages.clear();
            self.streaming = false;
        }
        fn push_system_message(&mut self, msg: String) {
            self.messages.push(msg);
        }
        fn clear_input(&mut self) {
            self.input.clear();
        }
        fn stop_streaming(&mut self) {
            self.streaming = false;
        }
        fn set_theme(&mut self, name: &str) -> bool {
            if let Some(&result) = self.theme_set_results.get(name) {
                if result {
                    self.current_theme = name.to_string();
                }
                return result;
            }
            if self.theme_names_vec.iter().any(|n| n == name) {
                self.current_theme = name.to_string();
                true
            } else {
                false
            }
        }
        fn reload_themes(&mut self) -> Result<usize, Vec<String>> {
            Ok(self.theme_names_vec.len())
        }
    }

    fn status_info() -> StatusInfo {
        StatusInfo {
            running: true,
            vault_path: "/home/vault".into(),
            uptime_seconds: 7200,
            model_name: Some("deepseek-chat".into()),
            token_usage_total: Some(5000),
            context_window: Some(128000),
            dispatcher_attached: true,
            orchestrator_attached: true,
            reactions_seen: 42,
        }
    }

    // ── Parse tests ──

    #[test]
    fn parse_quit() {
        assert_eq!(parse("/quit"), Some(SlashCommand::Quit));
        assert_eq!(parse("/exit"), Some(SlashCommand::Exit));
    }

    #[test]
    fn parse_new() {
        assert_eq!(parse("/new"), Some(SlashCommand::New));
    }

    #[test]
    fn parse_clear() {
        assert_eq!(parse("/clear"), Some(SlashCommand::Clear));
    }

    #[test]
    fn parse_help() {
        assert_eq!(parse("/help"), Some(SlashCommand::Help));
    }

    #[test]
    fn parse_status() {
        assert_eq!(parse("/status"), Some(SlashCommand::Status));
    }

    #[test]
    fn parse_model() {
        assert_eq!(parse("/model"), Some(SlashCommand::Model));
    }

    #[test]
    fn parse_fork() {
        assert_eq!(
            parse("/fork"),
            Some(SlashCommand::Fork { after_turn: None })
        );
        // `/fork 3` = go back to just after your 3rd turn.
        assert_eq!(
            parse("/fork 3"),
            Some(SlashCommand::Fork {
                after_turn: Some(3)
            })
        );
        // A typo'd argument must not silently fork at some *other* point than the one asked for —
        // falling back to the whole conversation is the only non-destructive reading, and the
        // original survives either way.
        assert_eq!(
            parse("/fork banana"),
            Some(SlashCommand::Fork { after_turn: None })
        );
    }

    #[test]
    fn parse_theme_list() {
        assert_eq!(parse("/theme"), Some(SlashCommand::Theme(ThemeCmd::List)));
        assert_eq!(
            parse("/theme list"),
            Some(SlashCommand::Theme(ThemeCmd::List))
        );
    }

    #[test]
    fn parse_theme_set() {
        assert_eq!(
            parse("/theme set dark"),
            Some(SlashCommand::Theme(ThemeCmd::Set("dark".into())))
        );
    }

    #[test]
    fn parse_theme_set_no_value() {
        assert_eq!(
            parse("/theme set"),
            Some(SlashCommand::Theme(ThemeCmd::Set("".into())))
        );
    }

    #[test]
    fn parse_theme_reload() {
        assert_eq!(
            parse("/theme reload"),
            Some(SlashCommand::Theme(ThemeCmd::Reload))
        );
    }

    #[test]
    fn parse_theme_implicit_set() {
        assert_eq!(
            parse("/theme dark"),
            Some(SlashCommand::Theme(ThemeCmd::Set("dark".into())))
        );
    }

    #[test]
    fn bare_session_is_an_alias_for_the_unified_switcher() {
        // `/session` and `/session list` both open the same unified switcher as `/sessions`.
        assert_eq!(parse("/session"), Some(SlashCommand::Sessions));
        assert_eq!(parse("/session list"), Some(SlashCommand::Sessions));
        assert_eq!(parse("/sessions"), Some(SlashCommand::Sessions));
    }

    #[test]
    fn parse_session_info() {
        assert_eq!(
            parse("/session info"),
            Some(SlashCommand::Session(SessionCmd::Info))
        );
    }

    #[test]
    fn parse_session_switch() {
        assert_eq!(
            parse("/session switch abc123"),
            Some(SlashCommand::Session(SessionCmd::Switch("abc123".into())))
        );
    }

    #[test]
    fn parse_session_close() {
        assert_eq!(
            parse("/session close"),
            Some(SlashCommand::Session(SessionCmd::Close))
        );
    }

    #[test]
    fn parse_session_unknown() {
        assert_eq!(
            parse("/session bogus"),
            Some(SlashCommand::Session(SessionCmd::Unknown("bogus".into())))
        );
    }

    #[test]
    fn parse_non_slash_returns_none() {
        assert_eq!(parse("hello"), None);
        assert_eq!(parse("not a command"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn parse_unknown_slash_command_returns_none() {
        assert_eq!(parse("/bogus"), None);
        assert_eq!(parse("/unknown-command"), None);
    }

    #[test]
    fn parse_trailing_spaces() {
        assert_eq!(parse("  /help  "), Some(SlashCommand::Help));
    }

    // ── Dispatch tests ──

    #[test]
    fn dispatch_quit() {
        let mut ctx = MockContext::new();
        ctx.input = "/quit".into();
        let results = crate::dispatch::dispatch(&SlashCommand::Quit, &mut ctx);
        assert_eq!(results, vec![CommandResult::Quit]);
        assert!(ctx.input.is_empty());
    }

    #[test]
    fn dispatch_new_not_streaming() {
        let mut ctx = MockContext::new();
        ctx.session = Some("sess-1".into());
        ctx.messages.push("old msg".into());
        let results = crate::dispatch::dispatch(&SlashCommand::New, &mut ctx);
        assert_eq!(
            results,
            vec![CommandResult::NewConversation {
                was_streaming: false
            }]
        );
        assert!(ctx.session.is_none());
        assert!(ctx.messages.is_empty());
        assert!(!ctx.streaming);
        assert!(ctx.input.is_empty());
    }

    #[test]
    fn dispatch_new_during_streaming() {
        let mut ctx = MockContext::new();
        ctx.session = Some("sess-1".into());
        ctx.streaming = true;
        let results = crate::dispatch::dispatch(&SlashCommand::New, &mut ctx);
        assert_eq!(
            results,
            vec![CommandResult::NewConversation {
                was_streaming: true
            }]
        );
        assert!(ctx.session.is_none());
        assert!(!ctx.streaming);
    }

    #[test]
    fn dispatch_clear() {
        let mut ctx = MockContext::new();
        ctx.messages.push("msg1".into());
        ctx.messages.push("msg2".into());
        ctx.input = "some input".into();
        let results = crate::dispatch::dispatch(&SlashCommand::Clear, &mut ctx);
        assert_eq!(results, vec![CommandResult::ChatCleared]);
        assert!(ctx.messages.is_empty());
        assert!(ctx.input.is_empty());
    }

    #[test]
    fn dispatch_help() {
        let mut ctx = MockContext::new();
        let results = crate::dispatch::dispatch(&SlashCommand::Help, &mut ctx);
        assert_eq!(results, vec![CommandResult::HelpShown]);
        assert_eq!(ctx.messages.len(), 1);
        assert!(ctx.messages[0].contains("/quit"));
        assert!(ctx.messages[0].contains("/help"));
        assert!(ctx.messages[0].contains("/status"));
        assert!(!ctx.messages[0].contains("Ctrl+C")); // no keybindings
    }

    #[test]
    fn dispatch_status_with_info() {
        let mut ctx = MockContext::new();
        ctx.status = Some(status_info());
        let results = crate::dispatch::dispatch(&SlashCommand::Status, &mut ctx);
        assert_eq!(results, vec![CommandResult::StatusShown]);
        assert_eq!(ctx.messages.len(), 1);
        assert!(ctx.messages[0].contains("deepseek-chat"));
        assert!(ctx.messages[0].contains("5000"));
        assert!(ctx.messages[0].contains("128000"));
        assert!(ctx.messages[0].contains("running"));
    }

    #[test]
    fn dispatch_status_no_connection() {
        let mut ctx = MockContext::new();
        let results = crate::dispatch::dispatch(&SlashCommand::Status, &mut ctx);
        assert_eq!(results, vec![CommandResult::StatusShown]);
        assert_eq!(ctx.messages.len(), 1);
        assert!(ctx.messages[0].contains("Not connected"));
    }

    #[test]
    fn dispatch_theme_set_valid() {
        let mut ctx = MockContext::new();
        let results = crate::dispatch::dispatch(
            &SlashCommand::Theme(ThemeCmd::Set("light".into())),
            &mut ctx,
        );
        assert_eq!(
            results,
            vec![CommandResult::ThemeChanged {
                name: "light".into()
            }]
        );
        assert_eq!(ctx.current_theme, "light");
    }

    #[test]
    fn dispatch_theme_set_invalid() {
        let mut ctx = MockContext::new();
        let results = crate::dispatch::dispatch(
            &SlashCommand::Theme(ThemeCmd::Set("bogus".into())),
            &mut ctx,
        );
        assert_eq!(results, vec![CommandResult::None]);
        assert_eq!(ctx.messages.len(), 1);
        assert!(ctx.messages[0].contains("Unknown theme"));
    }

    #[test]
    fn dispatch_theme_set_empty() {
        let mut ctx = MockContext::new();
        let results =
            crate::dispatch::dispatch(&SlashCommand::Theme(ThemeCmd::Set("".into())), &mut ctx);
        assert_eq!(results, vec![CommandResult::None]);
        assert_eq!(ctx.messages.len(), 1);
        assert!(ctx.messages[0].contains("Usage"));
    }

    #[test]
    fn dispatch_theme_list() {
        let mut ctx = MockContext::new();
        let results = crate::dispatch::dispatch(&SlashCommand::Theme(ThemeCmd::List), &mut ctx);
        assert!(matches!(results[0], CommandResult::ShowOptions { .. }));
        if let CommandResult::ShowOptions { title, options } = &results[0] {
            assert_eq!(title, "Available themes");
            assert_eq!(options.len(), 2);
        }
    }

    #[test]
    fn dispatch_theme_reload() {
        let mut ctx = MockContext::new();
        let results = crate::dispatch::dispatch(&SlashCommand::Theme(ThemeCmd::Reload), &mut ctx);
        assert_eq!(
            results,
            vec![CommandResult::ThemesReloaded {
                count: 2,
                errors: vec![]
            }]
        );
    }

    #[test]
    fn dispatch_model_with_info() {
        let mut ctx = MockContext::new();
        ctx.status = Some(status_info());
        let results = crate::dispatch::dispatch(&SlashCommand::Model, &mut ctx);
        assert_eq!(
            results,
            vec![
                CommandResult::ModelInfoShown,
                CommandResult::OpenModelBrowser,
            ]
        );
        assert_eq!(ctx.messages.len(), 1);
        assert!(ctx.messages[0].contains("deepseek-chat"));
    }

    #[test]
    fn dispatch_model_no_status() {
        let mut ctx = MockContext::new();
        let results = crate::dispatch::dispatch(&SlashCommand::Model, &mut ctx);
        assert_eq!(
            results,
            vec![
                CommandResult::ModelInfoShown,
                CommandResult::OpenModelBrowser,
            ]
        );
        assert_eq!(ctx.messages.len(), 1);
        assert!(ctx.messages[0].contains("Not connected"));
    }

    #[test]
    fn dispatch_session_close() {
        let mut ctx = MockContext::new();
        ctx.session = Some("sess-1".into());
        let results =
            crate::dispatch::dispatch(&SlashCommand::Session(SessionCmd::Close), &mut ctx);
        assert_eq!(
            results,
            vec![CommandResult::SessionClosed {
                id: Some("sess-1".into())
            }]
        );
        assert!(ctx.session.is_none());
    }

    #[test]
    fn dispatch_session_close_no_session() {
        let mut ctx = MockContext::new();
        let results =
            crate::dispatch::dispatch(&SlashCommand::Session(SessionCmd::Close), &mut ctx);
        assert_eq!(results, vec![CommandResult::SessionClosed { id: None }]);
        assert_eq!(ctx.messages.len(), 1);
        assert!(ctx.messages[0].contains("No active session"));
    }

    #[test]
    fn dispatch_session_switch() {
        let mut ctx = MockContext::new();
        ctx.conversations
            .push(("abc123".into(), "Test Conv".into(), None));
        let results = crate::dispatch::dispatch(
            &SlashCommand::Session(SessionCmd::Switch("abc".into())),
            &mut ctx,
        );
        assert_eq!(
            results,
            vec![CommandResult::SessionSwitched {
                id: "abc123".into()
            }]
        );
    }

    #[test]
    fn dispatch_session_switch_empty() {
        let mut ctx = MockContext::new();
        let results = crate::dispatch::dispatch(
            &SlashCommand::Session(SessionCmd::Switch("".into())),
            &mut ctx,
        );
        assert_eq!(results, vec![CommandResult::None]);
        assert!(ctx.messages[0].contains("Usage"));
    }

    #[test]
    fn dispatch_session_switch_no_match() {
        let mut ctx = MockContext::new();
        let results = crate::dispatch::dispatch(
            &SlashCommand::Session(SessionCmd::Switch("xyz".into())),
            &mut ctx,
        );
        assert_eq!(
            results,
            vec![CommandResult::SessionSwitched { id: "xyz".into() }]
        );
    }

    #[test]
    fn dispatch_session_info() {
        let mut ctx = MockContext::new();
        ctx.session = Some("c1".into());
        ctx.conversations
            .push(("c1".into(), "test conv".into(), None));
        let results = crate::dispatch::dispatch(&SlashCommand::Session(SessionCmd::Info), &mut ctx);
        assert_eq!(results, vec![CommandResult::SessionInfoShown]);
        assert!(ctx.messages[0].contains("c1"));
        assert!(ctx.messages[0].contains("test conv"));
    }

    #[test]
    fn dispatch_session_info_no_session() {
        let mut ctx = MockContext::new();
        let results = crate::dispatch::dispatch(&SlashCommand::Session(SessionCmd::Info), &mut ctx);
        assert_eq!(results, vec![CommandResult::None]);
        assert!(ctx.messages[0].contains("No active session"));
    }

    #[test]
    fn dispatch_session_list() {
        let mut ctx = MockContext::new();
        ctx.conversations.push(("c1".into(), "a".into(), None));
        ctx.conversations.push(("c2".into(), "b".into(), None));
        let results = crate::dispatch::dispatch(&SlashCommand::Session(SessionCmd::List), &mut ctx);
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], CommandResult::OpenSessionBrowser));
    }

    #[test]
    fn dispatch_session_unknown() {
        let mut ctx = MockContext::new();
        let results = crate::dispatch::dispatch(
            &SlashCommand::Session(SessionCmd::Unknown("bogus".into())),
            &mut ctx,
        );
        assert_eq!(results, vec![CommandResult::None]);
        assert!(ctx.messages[0].contains("Unknown session command"));
    }

    #[test]
    fn dispatch_fork_with_session() {
        let mut ctx = MockContext::new();
        ctx.session = Some("c1".into());
        let results = crate::dispatch::dispatch(&SlashCommand::Fork { after_turn: None }, &mut ctx);
        assert_eq!(
            results,
            vec![CommandResult::ForkRequested {
                parent_id: "c1".into(),
                after_turn: None,
            }]
        );
        assert!(ctx.messages[0].contains("Forking"));
        // The promise the human most needs to hear, since a fork sounds like it might move them.
        assert!(ctx.messages[0].contains("original stays put"));
    }

    #[test]
    fn dispatch_fork_at_a_turn_carries_the_turn_through() {
        let mut ctx = MockContext::new();
        ctx.session = Some("c1".into());
        let results = crate::dispatch::dispatch(
            &SlashCommand::Fork {
                after_turn: Some(2),
            },
            &mut ctx,
        );
        assert_eq!(
            results,
            vec![CommandResult::ForkRequested {
                parent_id: "c1".into(),
                after_turn: Some(2),
            }]
        );
    }

    #[test]
    fn dispatch_fork_at_turn_zero_explains_rather_than_forking() {
        // Turn 0 means "keep none of it", which is just a new conversation — not what was asked.
        let mut ctx = MockContext::new();
        ctx.session = Some("c1".into());
        let results = crate::dispatch::dispatch(
            &SlashCommand::Fork {
                after_turn: Some(0),
            },
            &mut ctx,
        );
        assert_eq!(results, vec![CommandResult::None]);
        assert!(ctx.messages[0].contains("numbered from 1"));
    }

    #[test]
    fn dispatch_fork_no_session() {
        let mut ctx = MockContext::new();
        let results = crate::dispatch::dispatch(&SlashCommand::Fork { after_turn: None }, &mut ctx);
        assert_eq!(results, vec![CommandResult::None]);
        assert!(ctx.messages[0].contains("No conversation to fork"));
    }

    // ── Display tests ──

    #[test]
    fn slash_command_display() {
        assert_eq!(SlashCommand::Quit.to_string(), "/quit");
        assert_eq!(SlashCommand::New.to_string(), "/new");
        assert_eq!(
            SlashCommand::Theme(ThemeCmd::Set("dark".into())).to_string(),
            "/theme set dark"
        );
        assert_eq!(
            SlashCommand::Session(SessionCmd::Switch("abc".into())).to_string(),
            "/session switch abc"
        );
    }

    // ── Uptime format tests ──

    #[test]
    fn format_uptime_hours_and_minutes() {
        assert_eq!(crate::format::format_uptime(7200), "2h 0m");
        assert_eq!(crate::format::format_uptime(3660), "1h 1m");
    }

    #[test]
    fn format_uptime_minutes_only() {
        assert_eq!(crate::format::format_uptime(120), "2m 0s");
        assert_eq!(crate::format::format_uptime(65), "1m 5s");
    }

    #[test]
    fn format_uptime_zero() {
        assert_eq!(crate::format::format_uptime(0), "0m 0s");
    }
}

// ── /goal parsing (S2/G2) ───────────────────────────────────────────────────

#[cfg(test)]
mod goal_command_tests {
    use crate::commands::{GoalCmd, SlashCommand};
    use crate::dispatch::parse;

    fn goal(input: &str) -> GoalCmd {
        match parse(input) {
            Some(SlashCommand::Goal(g)) => g,
            other => panic!("{input:?} did not parse as /goal: {other:?}"),
        }
    }

    #[test]
    fn bare_goal_opens_the_view() {
        assert_eq!(goal("/goal"), GoalCmd::View);
        assert_eq!(goal("/goal   "), GoalCmd::View);
    }

    #[test]
    fn lifecycle_subcommands_parse() {
        assert_eq!(goal("/goal status"), GoalCmd::Status);
        assert_eq!(goal("/goal pause"), GoalCmd::Pause);
        assert_eq!(goal("/goal clear"), GoalCmd::Clear);
        assert_eq!(goal("/goal resume"), GoalCmd::Resume(String::new()));
        assert_eq!(
            goal("/goal resume use postgres"),
            GoalCmd::Resume("use postgres".into())
        );
    }

    #[test]
    fn free_text_starts_a_goal() {
        assert_eq!(
            goal("/goal add a --version flag to the CLI"),
            GoalCmd::Start {
                project: None,
                text: "add a --version flag to the CLI".into()
            }
        );
    }

    #[test]
    fn in_names_an_explicit_project() {
        assert_eq!(
            goal("/goal in liberado add a --version flag"),
            GoalCmd::Start {
                project: Some("liberado".into()),
                text: "add a --version flag".into()
            }
        );
    }

    /// The reserved words are reserved as the FIRST word only. Refusing to start a goal because
    /// its text happens to begin with "status" would be a worse failure than the ambiguity.
    #[test]
    fn lifecycle_words_are_only_reserved_in_first_position() {
        assert_eq!(
            goal("/goal add a status endpoint"),
            GoalCmd::Start {
                project: None,
                text: "add a status endpoint".into()
            }
        );
        assert_eq!(
            goal("/goal in api clear the stale cache"),
            GoalCmd::Start {
                project: Some("api".into()),
                text: "clear the stale cache".into()
            }
        );
    }

    /// `/goal in <project>` with nothing after it is incomplete, not a goal named after a project.
    /// The handler prints usage for empty text rather than starting an empty goal.
    #[test]
    fn in_without_text_yields_empty_text_not_a_goal_named_after_the_project() {
        assert_eq!(
            goal("/goal in liberado"),
            GoalCmd::Start {
                project: Some("liberado".into()),
                text: String::new()
            }
        );
    }

    #[test]
    fn display_round_trips_through_parse() {
        for input in [
            "/goal",
            "/goal status",
            "/goal pause",
            "/goal clear",
            "/goal resume use postgres",
            "/goal add a --version flag",
            "/goal in liberado add a --version flag",
        ] {
            let parsed = parse(input).expect("parses");
            assert_eq!(
                parsed.to_string(),
                input,
                "Display must round-trip {input:?}"
            );
        }
    }
}
