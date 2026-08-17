use chat_client_contract::{ConvHeader, DaemonStatus};
use liberado_commands::{CommandContext, CommandResult, SlashCommand, StatusInfo};

use super::chat::ChatMsg;

/// Snapshot of the chat state a slash command needs to read, plus somewhere to record the
/// mutations handlers make (`ctx.push_system_message`, `ctx.set_active_session`, ...). The caller
/// (`chat.rs`) applies `messages`/`session_id` back onto the real Dioxus signals after dispatch —
/// `CommandContext`'s `&self` read methods can't return borrows tied to a `Signal`'s transient
/// `read()` guard, so a snapshot-then-reapply is the shape the trait requires.
struct WebCommandContext {
    messages: Vec<ChatMsg>,
    session_id: Option<String>,
    sending: bool,
    message_count: usize,
    conversations: Vec<ConvHeader>,
    status: Option<DaemonStatus>,
    /// Active theme name. `set_theme` validates against the shared registry and rewrites this; the
    /// caller reads the new name off `CommandResult::ThemeChanged`.
    theme: String,
}

impl CommandContext for WebCommandContext {
    fn active_session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
    fn is_streaming(&self) -> bool {
        self.sending
    }
    fn conversation_count(&self) -> usize {
        self.conversations.len()
    }
    fn find_conversation_id_by_prefix(&self, prefix: &str) -> Option<String> {
        self.conversations
            .iter()
            .find(|c| c.id.starts_with(prefix))
            .map(|c| c.id.clone())
    }
    fn status_info(&self) -> Option<StatusInfo> {
        self.status.as_ref().map(|s| StatusInfo {
            running: s.running,
            vault_path: s.vault_path.clone(),
            uptime_seconds: s.uptime_seconds,
            model_name: s.model_name.clone(),
            token_usage_total: s.token_usage_total,
            context_window: s.context_window,
            dispatcher_attached: s.dispatcher_attached,
            orchestrator_attached: s.orchestrator_attached,
            reactions_seen: s.reactions_seen,
        })
    }
    fn theme_names(&self) -> Vec<String> {
        // The shared registry's built-ins — the same dark/light/nord the TUI offers. User theme
        // *files* are absent by construction: the registry reads them from
        // `<config>/liberado/themes/*.toml` and a WASM build has no filesystem, so this surface sees
        // built-ins only. Adding a built-in upstream reaches both surfaces with no change here.
        crate::theme::theme_names()
    }
    fn current_theme_name(&self) -> &str {
        &self.theme
    }
    fn conversation_title_for(&self, id: &str) -> Option<String> {
        self.conversations
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| c.title.clone())
            .filter(|t| !t.is_empty())
    }
    fn conversation_parent_for(&self, _id: &str) -> Option<String> {
        // `ConvHeader` doesn't carry lineage over the wire today (only the DB/conversation-store
        // side does) — honestly report "unknown" rather than guessing.
        None
    }
    fn message_count(&self) -> usize {
        self.message_count
    }
    fn conversation_list(&self) -> Vec<(String, String)> {
        self.conversations
            .iter()
            .map(|c| {
                let title = c
                    .title
                    .clone()
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "(untitled)".into());
                (title, c.id.clone())
            })
            .collect()
    }

    fn set_active_session(&mut self, id: Option<String>) {
        self.session_id = id;
    }
    fn clear_chat(&mut self) {
        self.messages.clear();
    }
    fn reset_for_new_conversation(&mut self) {
        self.session_id = None;
        self.messages.clear();
    }
    fn push_system_message(&mut self, msg: String) {
        self.messages.push(ChatMsg {
            role: "system",
            content: msg,
            thinking_steps: Vec::new(),
        });
    }
    fn clear_input(&mut self) {}
    fn stop_streaming(&mut self) {}
    fn set_theme(&mut self, name: &str) -> bool {
        // Accept only names the registry actually knows, so `/theme set bogus` reports failure
        // instead of leaving the UI on a theme that silently fell back to dark.
        if crate::theme::theme_names().iter().any(|n| n == name) {
            self.theme = name.to_string();
            true
        } else {
            false
        }
    }
    fn reload_themes(&mut self) -> Result<usize, Vec<String>> {
        // Honest refusal rather than a fake success. Reloading means re-reading
        // `<config>/liberado/themes/*.toml`, and a browser cannot read the filesystem — the built-ins
        // are compiled in, so there is nothing to re-read either. Serving user theme files here would
        // need the daemon to expose them over HTTP.
        Err(vec![
            "Reloading themes needs the config directory, which the browser cannot read.              Built-in themes (see /theme list) are always available; user theme files are TUI-only."
                .into(),
        ])
    }
}

async fn fetch_conversations(api_base: &str) -> Vec<ConvHeader> {
    let url = format!("{api_base}/api/conversations");
    match reqwest::get(&url).await {
        Ok(resp) => resp.json::<Vec<ConvHeader>>().await.unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

async fn fetch_status(api_base: &str) -> Option<DaemonStatus> {
    let url = format!("{api_base}/api/status");
    let resp = reqwest::get(&url).await.ok()?;
    resp.json::<DaemonStatus>().await.ok()
}

fn parse_error(text: &str) -> (Vec<ChatMsg>, Option<String>, Vec<CommandResult>) {
    let msg = ChatMsg {
        role: "system",
        content: format!("Unknown command: {text}. Type /help for available commands."),
        thinking_steps: Vec::new(),
    };
    (vec![msg], None, Vec::new())
}

/// Render a `ShowOptions` result (from `/theme list`, `/session list`) as a plain system message —
/// the web UI has no modal/picker widget yet, so a numbered list is the honest fallback.
fn render_options(title: &str, options: &[(String, String)]) -> String {
    let mut out = format!("{title}:\n");
    for (label, _id) in options {
        out.push_str("  ");
        out.push_str(label);
        out.push('\n');
    }
    out
}

/// Run a slash command against a snapshot of the current chat state. Fetches the conversation list
/// or daemon status over HTTP only when the command actually needs it (`/session ...`, `/status`,
/// `/model`), so `/new`, `/clear`, `/help`, etc. stay instant with no network round trip.
pub async fn handle_slash_command(
    text: &str,
    api_base: &str,
    session_id: Option<String>,
    sending: bool,
    message_count: usize,
    current_theme: &str,
) -> (Vec<ChatMsg>, Option<String>, Vec<CommandResult>) {
    let cmd = match liberado_commands::parse(text) {
        Some(c) => c,
        None => return parse_error(text),
    };

    let (conversations, status) = match &cmd {
        SlashCommand::Session(_) => (fetch_conversations(api_base).await, None),
        SlashCommand::Status | SlashCommand::Model => (Vec::new(), fetch_status(api_base).await),
        _ => (Vec::new(), None),
    };

    let mut ctx = WebCommandContext {
        messages: Vec::new(),
        session_id,
        sending,
        message_count,
        conversations,
        status,
        theme: current_theme.to_string(),
    };

    let results = liberado_commands::dispatch(&cmd, &mut ctx);
    // `ShowOptions` is the list a text-only surface prints. When the same dispatch also asks for a
    // picker, that picker *is* the list — printing both would show it twice.
    let opens_picker = results
        .iter()
        .any(|r| matches!(r, CommandResult::OpenThemeBrowser));
    for result in &results {
        if let CommandResult::ShowOptions { title, options } = result
            && !opens_picker
        {
            ctx.push_system_message(render_options(title, options));
        }
    }
    (ctx.messages, ctx.session_id, results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chat_client_contract::{ConvHeader, DaemonStatus};
    use liberado_commands::CommandResult;

    const BASE: &str = "http://daemon.test";

    fn conv(id: &str, title: Option<&str>) -> ConvHeader {
        ConvHeader {
            id: id.into(),
            title: title.map(str::to_string),
            ..Default::default()
        }
    }

    /// A context preloaded the way the chat would snapshot it — used for the trait-contract tests
    /// below, which exercise the `CommandContext` implementation directly.
    fn ctx_with(conversations: Vec<ConvHeader>) -> WebCommandContext {
        WebCommandContext {
            messages: Vec::new(),
            session_id: None,
            sending: false,
            message_count: 0,
            conversations,
            status: None,
            theme: "dark".to_string(),
        }
    }

    async fn run(
        text: &str,
        session: Option<String>,
        sending: bool,
        theme: &str,
    ) -> (Vec<ChatMsg>, Option<String>, Vec<CommandResult>) {
        handle_slash_command(text, BASE, session, sending, 0, theme).await
    }

    /// An unknown command is reported as such — the input is not silently dropped.
    #[tokio::test]
    async fn unknown_commands_are_reported() {
        let (msgs, session, results) = run("hello world", None, false, "dark").await;
        assert!(results.is_empty());
        assert_eq!(session, None);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "system");
        assert!(
            msgs[0].content.contains("Unknown command: hello world"),
            "{}",
            msgs[0].content
        );
    }

    /// `/new` resets the session and reports that a new conversation should open.
    #[tokio::test]
    async fn new_resets_and_reports() {
        let (msgs, session, results) = run("/new", Some("01ABC".to_string()), false, "dark").await;
        assert_eq!(session, None, "session must be cleared");
        assert!(msgs.is_empty());
        assert_eq!(
            results,
            vec![CommandResult::NewConversation {
                was_streaming: false
            }]
        );
    }

    /// The streaming flag rides the result so the caller knows whether a turn was in flight.
    #[tokio::test]
    async fn new_reports_whether_it_killed_a_stream() {
        let (_, _, results) = run("/new", None, true, "dark").await;
        assert_eq!(
            results,
            vec![CommandResult::NewConversation {
                was_streaming: true
            }]
        );
    }

    #[tokio::test]
    async fn clear_reports_the_clear() {
        let (msgs, session, results) =
            run("/clear", Some("01ABC".to_string()), false, "dark").await;
        assert_eq!(results, vec![CommandResult::ChatCleared]);
        assert!(msgs.is_empty());
        // `/clear` clears the transcript, not the session.
        assert_eq!(session.as_deref(), Some("01ABC"));
    }

    /// `/help` both announces and explains — the message is the catalog's, rendered into the
    /// conversation.
    #[tokio::test]
    async fn help_renders_the_catalog() {
        let (msgs, _, results) = run("/help", None, false, "dark").await;
        assert_eq!(results, vec![CommandResult::HelpShown]);
        let joined: String = msgs.iter().map(|m| m.content.clone()).collect();
        assert!(joined.contains("Slash commands"), "{joined}");
        assert!(joined.contains("/help"), "{joined}");
    }

    /// A valid `/theme set` validates against the shared registry and reports the change.
    #[tokio::test]
    async fn theme_set_accepts_a_known_theme() {
        let (msgs, _, results) = run("/theme set nord", None, false, "dark").await;
        assert_eq!(
            results,
            vec![CommandResult::ThemeChanged {
                name: "nord".into()
            }]
        );
        assert!(
            msgs.iter().any(|m| m.content == "Theme: nord"),
            "expected a 'Theme: nord' system message"
        );
    }

    /// An unknown theme is refused — no `ThemeChanged`, and the refusal names the available set so
    /// the typo is self-correcting.
    #[tokio::test]
    async fn theme_set_refuses_an_unknown_theme() {
        let (msgs, _, results) = run("/theme set bogus", None, false, "dark").await;
        assert_eq!(results, vec![CommandResult::None]);
        let joined: String = msgs.iter().map(|m| m.content.clone()).collect();
        assert!(joined.contains("Unknown theme: bogus"), "{joined}");
        assert!(
            joined.contains("dark") && joined.contains("nord"),
            "{joined}"
        );
    }

    /// `/theme list` emits the picker cue alongside the options; the webui prints the options only
    /// when no picker is opening — asserting the `opens_picker` suppression from above.
    #[tokio::test]
    async fn theme_list_opens_the_picker_without_duplicating_the_list() {
        let (msgs, _, results) = run("/theme list", None, false, "dark").await;
        assert!(results.contains(&CommandResult::OpenThemeBrowser));
        assert!(
            results
                .iter()
                .any(|r| matches!(r, CommandResult::ShowOptions { .. })),
            "options must still ride the results for text surfaces: {results:?}"
        );
        assert!(
            msgs.is_empty(),
            "with a picker opening, the options must not also be printed"
        );
    }

    /// `render_options` prints the labels only — ids are machine values and would drown the list.
    #[test]
    fn render_options_prints_labels_only() {
        let out = render_options(
            "Available themes",
            &[
                ("  dark  (active)".to_string(), "dark".to_string()),
                ("    light".to_string(), "light".to_string()),
            ],
        );
        assert!(out.starts_with("Available themes:\n"), "{out}");
        assert!(
            out.contains("  dark  (active)") && out.contains("    light"),
            "{out}"
        );
        assert!(
            !out.contains("dark\n"),
            "the id column must not print: {out:?}"
        );
    }

    /// `parse_error` is the one message shape an unknown command can produce — pinned so the copy
    /// (which tells the user how to recover) cannot drift from what is actually offered.
    #[test]
    fn parse_error_points_at_help() {
        let (msgs, session, results) = parse_error("wat");
        assert_eq!(session, None);
        assert!(results.is_empty());
        assert!(msgs[0].content.contains("/help"), "{}", msgs[0].content);
    }

    // ── WebCommandContext contract ────────────────────────────────────────

    /// Prefix matching resolves an unambiguous id; `None` when nothing matches — the contract the
    /// `/session` subcommands rely on.
    #[test]
    fn prefix_matching_finds_the_conversation() {
        let ctx = ctx_with(vec![conv("01HZABC", None), conv("01HZXYZ", None)]);
        assert_eq!(
            ctx.find_conversation_id_by_prefix("01HZA"),
            Some("01HZABC".into())
        );
        assert_eq!(ctx.find_conversation_id_by_prefix("nope"), None);
        assert_eq!(
            ctx.find_conversation_id_by_prefix("01HZ"),
            Some("01HZABC".into())
        );
    }

    /// An empty title reads as no title — the conversation list then labels it "(untitled)" rather
    /// than showing a blank row. Whitespace-only titles are not trimmed (the production code checks
    /// `!t.is_empty()`, not `!t.trim().is_empty()`), so they pass through.
    #[test]
    fn empty_titles_read_as_untitled() {
        let ctx = ctx_with(vec![
            conv("01A", Some("  ")),
            conv("01B", None),
            conv("01C", Some("Real title")),
        ]);
        // Whitespace-only title passes through — the production code does not trim.
        assert_eq!(ctx.conversation_title_for("01A"), Some("  ".to_string()));
        assert_eq!(ctx.conversation_title_for("01B"), None);
        assert_eq!(ctx.conversation_title_for("01C"), Some("Real title".into()));
        let list = ctx.conversation_list();
        // For the list, the '  ' title is not empty, so it is used as-is.
        assert_eq!(list[0], ("  ".to_string(), "01A".to_string()));
        assert_eq!(list[2], ("Real title".to_string(), "01C".to_string()));
    }

    /// `set_theme` validates against the shared registry — a name that would silently fall back to
    /// dark must be refused, not recorded.
    #[test]
    fn set_theme_validates_against_the_registry() {
        let mut ctx = ctx_with(Vec::new());
        assert!(ctx.set_theme("light"));
        assert_eq!(ctx.theme, "light");
        assert!(!ctx.set_theme("bogus"));
        assert_eq!(
            ctx.theme, "light",
            "a refused set must not clobber the current theme"
        );
    }

    /// `reload_themes` refuses honestly — a browser cannot read the config directory, and faking a
    /// success would claim files were re-read.
    #[test]
    fn reload_themes_refuses_without_a_config_dir() {
        let mut ctx = ctx_with(Vec::new());
        match ctx.reload_themes() {
            Err(errors) => assert!(
                errors.iter().any(|e| e.contains("config directory")),
                "{errors:?}"
            ),
            Ok(n) => panic!("reload must not claim success, got {n}"),
        }
    }

    /// `reset_for_new_conversation` is what `/new` means at the context level: session gone and
    /// transcript gone, together.
    #[test]
    fn reset_clears_session_and_messages() {
        let mut ctx = ctx_with(Vec::new());
        ctx.session_id = Some("01ABC".into());
        ctx.messages.push(ChatMsg {
            role: "user",
            content: "hi".into(),
            thinking_steps: Vec::new(),
        });
        ctx.reset_for_new_conversation();
        assert_eq!(ctx.session_id, None);
        assert!(ctx.messages.is_empty());
    }

    /// `status_info` maps the wire status to the commands' view — the bridge the `/status` handler
    /// reads.
    #[test]
    fn status_info_maps_the_wire_status() {
        let status = DaemonStatus {
            running: true,
            vault_path: "/vault".into(),
            uptime_seconds: 42,
            watcher_active: true,
            dispatcher_attached: true,
            orchestrator_attached: false,
            reactions_seen: 7,
            model_name: Some("gpt-5".into()),
            token_usage_total: None,
            context_window: None,
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            enter_sends: true,
        };
        let mut ctx = ctx_with(Vec::new());
        ctx.status = Some(status);
        let info = ctx.status_info().unwrap();
        assert!(info.running);
        assert_eq!(info.vault_path, "/vault");
        assert_eq!(info.uptime_seconds, 42);
        assert_eq!(info.model_name.as_deref(), Some("gpt-5"));
        assert_eq!(info.reactions_seen, 7);
        assert!(ctx.status_info().is_some());
    }

    /// The snapshot's read methods mirror the fields they wrap — a command that reads one must see
    /// what the chat actually snapshot. Two conversations, so a literal in the impl cannot match by
    /// accident.
    #[test]
    fn context_reads_expose_the_snapshot() {
        let mut ctx = ctx_with(vec![conv("01A", None), conv("01B", None)]);
        ctx.session_id = Some("01A".into());
        ctx.message_count = 7;
        ctx.theme = "nord".into();
        assert_eq!(ctx.active_session_id(), Some("01A"));
        assert_eq!(ctx.message_count(), 7);
        assert_eq!(ctx.current_theme_name(), "nord");
        assert_eq!(ctx.conversation_count(), 2);
        // `ConvHeader` does not carry lineage over the wire; the context reports unknown rather
        // than guessing a parent that could route a fork to the wrong conversation.
        assert_eq!(ctx.conversation_parent_for("01A"), None);
    }

    /// The mutation methods change the snapshot the caller re-applies — `/new`, `/clear` and
    /// `/session switch` all depend on these taking effect.
    #[test]
    fn context_writes_update_the_snapshot() {
        let mut ctx = ctx_with(Vec::new());
        ctx.messages.push(ChatMsg {
            role: "user",
            content: "hi".into(),
            thinking_steps: Vec::new(),
        });
        ctx.session_id = Some("01A".into());

        ctx.set_active_session(None);
        assert_eq!(
            ctx.session_id, None,
            "set_active_session must replace the id"
        );

        ctx.clear_chat();
        assert!(
            ctx.messages.is_empty(),
            "clear_chat must drop the transcript"
        );
    }
}
