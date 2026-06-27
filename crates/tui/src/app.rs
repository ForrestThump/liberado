//! Application state machine for the Liberado TUI.
//!
//! `App` is the single source of truth for the terminal's state. It is mutated by
//! `App::update(action) → Vec<Effect>` for API/SSE events, and directly by keyboard
//! handlers. The returned `Effect` instructions drive side effects in `main.rs`.

use crossterm::event::MouseEvent;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use liberado_theme::{Theme, ThemeRegistry};
use ratatui::layout::Rect;
use std::collections::HashSet;

use crate::api::{
    ChatMessage, ConvHeader, DaemonStatus, ReactionEvent, ToolCallChip, ToolResultChip,
};
use crate::tuning::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The text input area at the bottom.
    Input,
    /// The conversation list in the right sidebar.
    SidebarConversations,
    /// Scrollable message history in the chat pane.
    ChatMessages,
}

/// A single chat message in the scrollback buffer.
#[derive(Debug, Clone)]
pub enum Message {
    /// User-typed input — rendered with cyan "> " prefix.
    User(String),
    /// Model reply — rendered through the markdown parser.
    Assistant(String),
    /// `[tool] name(args)` inline chip pushed during SSE streaming.
    ToolCall(ToolCallChip),
    /// `[tool] name ok|err preview` outcome chip.
    ToolResult(ToolResultChip),
    /// Italic gray status/error/command-output lines.
    System(String),
}

/// The application state machine.
///
/// Create with [`App::new()`]. Drive it via [`App::handle_key()`],
/// [`App::handle_mouse()`], and [`App::update()`]. All three return
/// `Vec<Effect>` — commands the caller must execute (spawn HTTP, cancel
/// stream, quit, etc.).
///
/// All fields are public so render functions can read them. Mutation
/// should always go through the three entry-point methods.
#[derive(Debug)]
pub struct App {
    pub server: String,
    pub session: Option<String>,
    pub messages: Vec<Message>,
    pub input: String,
    pub cursor: usize,
    pub streaming: bool,
    pub assistant_buf: String,
    pub status: Option<DaemonStatus>,
    pub reactions: Vec<ReactionEvent>,
    pub conversations: Vec<ConvHeader>,
    pub focus: Focus,
    pub sidebar_selection: usize,
    pub scroll_offset: usize,
    pub pending_load: Option<String>,
    pub sidebar_filter: String,
    pub theme: Theme,
    pub theme_registry: ThemeRegistry,
    pub daemon_connected: bool,
    pub collapsed_nodes: HashSet<String>,
    pub expanded_messages: HashSet<usize>,
    pub chat_cursor: usize,
    pub input_max_height: u16,
    pub input_scroll: usize,
    pub layout: LayoutRects,
}

/// Layout rectangles populated by the draw pass for mouse hit-testing.
#[derive(Debug, Clone, Default)]
pub struct LayoutRects {
    pub chat: Rect,
    pub sidebar_full: Rect,
    pub sidebar_conversations: Rect,
    pub input: Rect,
    /// The available character-width inside the input box (area width minus borders).
    pub input_content_width: usize,
}

/// A single node in the conversation tree, flattened for rendering.
#[derive(Debug, Clone)]
pub struct VisibleNode {
    pub header: ConvHeader,
    pub depth: usize,
    pub is_last: bool,
    pub has_children: bool,
    pub collapsed: bool,
    pub ancestors_last: Vec<bool>,
}

/// A snapshot of the application's status, computed on demand.
#[derive(Debug, Clone)]
pub struct StatusSummary {
    pub connected: bool,
    pub uptime: Option<String>,
    pub vault_path: Option<String>,
    pub message_count: usize,
    pub session_id: Option<String>,
    pub streaming: bool,
    pub model_name: Option<String>,
    pub token_usage_total: Option<u64>,
    pub context_window: Option<u64>,
}

impl App {
    pub fn new(server: String, registry: ThemeRegistry) -> Self {
        let theme = registry
            .get("dark")
            .cloned()
            .unwrap_or_else(Theme::default_dark);
        Self {
            server,
            session: None,
            messages: Vec::new(),
            input: String::new(),
            cursor: 0,
            streaming: false,
            assistant_buf: String::new(),
            status: None,
            reactions: Vec::new(),
            conversations: Vec::new(),
            focus: Focus::Input,
            sidebar_selection: 0,
            scroll_offset: 0,
            pending_load: None,
            sidebar_filter: String::new(),
            theme,
            theme_registry: registry,
            daemon_connected: false,
            collapsed_nodes: HashSet::new(),
            expanded_messages: HashSet::new(),
            chat_cursor: 0,
            input_max_height: INPUT_MAX_HEIGHT,
            input_scroll: 0,
            layout: LayoutRects::default(),
        }
    }

    pub fn status_summary(&self) -> StatusSummary {
        StatusSummary {
            connected: self.daemon_connected,
            uptime: self
                .status
                .as_ref()
                .map(|s| format_uptime(s.uptime_seconds)),
            vault_path: self.status.as_ref().map(|s| s.vault_path.clone()),
            message_count: self.messages.len(),
            session_id: self.session.clone(),
            streaming: self.streaming,
            model_name: self.status.as_ref().and_then(|s| s.model_name.clone()),
            token_usage_total: self.status.as_ref().and_then(|s| s.token_usage_total),
            context_window: self.status.as_ref().and_then(|s| s.context_window),
        }
    }

    /// The 0-based column within the current logical line that the cursor is on
    /// (characters since the last `\n`, or from the start).
    pub fn cursor_col(&self) -> usize {
        let line_start = self.input[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        self.input[line_start..self.cursor].chars().count()
    }

    pub fn cursor_visual_line(&self) -> usize {
        let cw = self.layout.input_content_width.max(1);
        let mut visual_line = 0usize;
        let mut byte_pos = 0usize;
        for logical in self.input.lines() {
            let line_end = byte_pos + logical.len();
            if self.cursor <= line_end {
                let col = self.input[byte_pos..self.cursor].chars().count();
                visual_line += col / cw;
                return visual_line;
            }
            let chars = logical.chars().count();
            visual_line += if chars == 0 { 1 } else { (chars + cw - 1) / cw };
            byte_pos = (line_end + 1).min(self.input.len());
        }
        visual_line
    }

    pub fn cursor_visual_col(&self) -> usize {
        let cw = self.layout.input_content_width.max(1);
        let line_start = self.input[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        self.input[line_start..self.cursor].chars().count() % cw
    }

    pub fn byte_offset_for_visual(&self, target_line: usize, target_col: usize) -> usize {
        let cw = self.layout.input_content_width.max(1);
        let mut visual_line = 0usize;
        let mut byte_pos = 0usize;
        for logical in self.input.lines() {
            let line_end = byte_pos + logical.len();
            let chars_in_logical = logical.chars().count();
            let visual_lines_in_logical =
                if chars_in_logical == 0 { 1 } else { (chars_in_logical + cw - 1) / cw };
            if target_line < visual_line + visual_lines_in_logical {
                let local_line = target_line.saturating_sub(visual_line);
                let start_char = local_line * cw;
                let end_char = (start_char + target_col).min(chars_in_logical);
                let byte_in_logical = logical
                    .char_indices()
                    .nth(end_char)
                    .map(|(i, _)| i)
                    .unwrap_or(logical.len());
                return (byte_pos + byte_in_logical).min(self.input.len());
            }
            visual_line += visual_lines_in_logical;
            byte_pos = (line_end + 1).min(self.input.len());
        }
        self.input.len()
    }

    pub fn input_visual_lines(&self) -> usize {
        let cw = self.layout.input_content_width;
        if cw == 0 {
            return self.input.lines().count().max(1);
        }
        self.input
            .lines()
            .map(|line| {
                let chars = line.chars().count();
                if chars == 0 { 1 } else { (chars + cw - 1) / cw }
            })
            .sum::<usize>()
            .max(1)
    }

    pub fn scroll_input_to_cursor(&mut self) {
        let max_visible = self.input_max_height.saturating_sub(2) as usize;
        if max_visible == 0 {
            return;
        }
        let cursor_line = self.cursor_visual_line();
        let total_lines = self.input_visual_lines();
        if cursor_line < self.input_scroll {
            self.input_scroll = cursor_line;
        } else if cursor_line >= self.input_scroll + max_visible {
            self.input_scroll = cursor_line.saturating_sub(max_visible - 1);
        }
        self.input_scroll = self.input_scroll.min(total_lines.saturating_sub(max_visible));
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            // Clear the input box if it has content, otherwise quit.
            // This makes double-tap Ctrl+C a reliable exit path.
            if self.focus == Focus::Input && !self.input.trim().is_empty() {
                self.input.clear();
                self.cursor = 0;
                return vec![Effect::None];
            }
            return vec![Effect::Quit];
        }
        if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.streaming {
                return self.stop_stream();
            }
            return vec![Effect::None];
        }
        match self.focus {
            Focus::Input => crate::handlers::input::handle(self, key),
            Focus::SidebarConversations => crate::handlers::sidebar::handle(self, key),
            Focus::ChatMessages => crate::handlers::chat::handle(self, key),
        }
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) -> Vec<Effect> {
        crate::handlers::mouse::handle(self, event)
    }

    pub(crate) fn handle_slash_command(&mut self, input: &str) -> Vec<Effect> {
        crate::commands::dispatch(self, input)
    }

    pub(crate) fn scroll_to_chat_cursor(&mut self) {
        let visible = CHAT_VISIBLE_LINES;
        let bottom = self.scroll_offset.saturating_add(visible).saturating_sub(1);
        if self.chat_cursor < self.scroll_offset {
            self.scroll_offset = self.chat_cursor;
        } else if self.chat_cursor > bottom && self.chat_cursor >= visible {
            self.scroll_offset = self.chat_cursor - visible + 1;
        }
    }

    pub(crate) fn clamp_sidebar_selection(&mut self) {
        let len = self.visible_conversations().len();
        if len > 0 && self.sidebar_selection >= len {
            self.sidebar_selection = len - 1;
        }
    }

    pub fn scroll_back(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(lines);
    }
    pub fn scroll_forward(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    fn system_msg(&mut self, msg: impl Into<String>, effect: Effect) -> Vec<Effect> {
        self.messages.push(Message::System(msg.into()));
        self.scroll_offset = 0;
        vec![effect]
    }

    pub fn visible_conversations(&self) -> Vec<VisibleNode> {
        crate::conversations::visible_tree(
            &self.conversations,
            &self.collapsed_nodes,
            &self.sidebar_filter,
        )
    }

    pub fn filtered_conversations(&self) -> Vec<&ConvHeader> {
        crate::conversations::filtered_list(&self.conversations, &self.sidebar_filter)
    }

    pub fn cancel_stream(&mut self) -> Vec<Effect> {
        self.end_stream("[cancelled]")
    }

    pub fn stop_stream(&mut self) -> Vec<Effect> {
        self.end_stream("[stopped]")
    }

    fn end_stream(&mut self, label: &str) -> Vec<Effect> {
        self.streaming = false;
        if !self.assistant_buf.is_empty() {
            self.messages
                .push(Message::Assistant(std::mem::take(&mut self.assistant_buf)));
        }
        self.messages.push(Message::System(label.into()));
        self.scroll_offset = 0;
        vec![Effect::CancelStream]
    }
}

/// Events that flow into [`App::update()`] from background tasks (poller, SSE stream).
///
/// Each variant represents a single atomic update. `App::update()` mutates state and
/// returns `Vec<Effect>` — it never spawns async work directly.
#[derive(Debug, Clone)]
pub enum Action {
    /// Daemon health + model info from `GET /api/status`.
    StatusUpdate(DaemonStatus),
    /// Recent file-change reactions from `GET /api/reactions`.
    ReactionsUpdate(Vec<ReactionEvent>),
    /// Conversation list from `GET /api/conversations`.
    ConversationsUpdate(Vec<ConvHeader>),
    /// Full message history loaded for a conversation.
    HistoryLoaded {
        id: String,
        messages: Vec<ChatMessage>,
    },
    /// Conversation session id from the first SSE event.
    SseSession(String),
    /// Streaming text delta.
    SseToken(String),
    /// Tool call started during streaming.
    SseTool { name: String, args: String },
    /// Tool call completed with outcome.
    SseToolResult {
        name: String,
        ok: bool,
        preview: String,
    },
    /// SSE stream finished normally.
    SseDone,
    /// SSE stream or HTTP call failed.
    SseFailed(String),
    /// Daemon connectivity transition (true = connected, false = lost).
    ConnectionStatus(bool),
    /// Periodic heartbeat from the poller (currently a no-op in `update()`).
    Tick,
}

/// Commands returned by [`App`] for the main loop / [`EffectRunner`] to execute.
///
/// These are side-effect descriptions — the App never performs I/O. The caller
/// (main loop or `effects::EffectRunner`) maps each variant to async work.
#[derive(Debug)]
pub enum Effect {
    StartChatStream {
        message: String,
        session: Option<String>,
    },
    RefreshConversations,
    LoadConversationHistory(String),
    CancelStream,
    ForkConversation(String),
    SetWindowTitle(String),
    Quit,
    None,
}

impl App {
    pub fn update(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::StatusUpdate(status) => {
                self.status = Some(status);
                vec![Effect::None]
            }
            Action::ReactionsUpdate(reactions) => {
                self.reactions = reactions;
                vec![Effect::None]
            }
            Action::ConversationsUpdate(convs) => {
                self.conversations = convs;
                if self.sidebar_selection >= self.conversations.len().saturating_sub(1) {
                    self.sidebar_selection = self.conversations.len().saturating_sub(1);
                }
                vec![Effect::None]
            }
            Action::HistoryLoaded { id, messages } => {
                if self.pending_load.as_deref() != Some(&id) {
                    return vec![Effect::None]; // stale — newer request superseded this one
                }
                self.session = Some(id);
                self.pending_load = None;
                self.messages.clear();
                self.chat_cursor = 0;
                self.expanded_messages.clear();
                for msg in messages {
                    match msg.role.as_str() {
                        "user" => self.messages.push(Message::User(msg.content)),
                        "assistant" => {
                            if let Some(tool_calls) = &msg.tool_calls {
                                self.push_tool_history(tool_calls);
                            }
                            if !msg.content.is_empty() {
                                self.messages.push(Message::Assistant(msg.content));
                            }
                        }
                        _ => {}
                    }
                }
                // Enforce the message cap on history load — this is the primary source of
                // unbounded growth (loading a conversation with thousands of messages).
                // Normal conversation turns typically stay well under MAX_MESSAGE_COUNT,
                // so we only prune here rather than on every user message push.
                if self.messages.len() > MAX_MESSAGE_COUNT {
                    let removed = self.messages.len() - MAX_MESSAGE_COUNT;
                    self.messages = self.messages.split_off(removed);
                    self.messages.insert(
                        0,
                        Message::System(format!("... {removed} earlier messages omitted")),
                    );
                }
                self.scroll_offset = 0;
                self.focus = Focus::Input;
                vec![Effect::None]
            }
            Action::SseSession(id) => {
                if self.session.is_none() {
                    self.session = Some(id);
                }
                vec![Effect::None]
            }
            Action::SseToken(token) => {
                self.assistant_buf.push_str(&token);
                vec![Effect::None]
            }
            Action::SseTool { name, args } => {
                self.messages
                    .push(Message::ToolCall(ToolCallChip { name, args }));
                vec![Effect::None]
            }
            Action::SseToolResult { name, ok, preview } => {
                self.messages
                    .push(Message::ToolResult(ToolResultChip { name, ok, preview }));
                vec![Effect::None]
            }
            Action::SseDone => {
                let finished = std::mem::take(&mut self.assistant_buf);
                if !finished.is_empty() {
                    self.messages.push(Message::Assistant(finished));
                }
                self.streaming = false;
                self.scroll_offset = 0;
                vec![Effect::RefreshConversations]
            }
            Action::SseFailed(err) => {
                self.pending_load = None;
                self.assistant_buf.clear();
                self.messages
                    .push(Message::System(format!("[error] {err}")));
                self.streaming = false;
                vec![Effect::RefreshConversations]
            }
            Action::ConnectionStatus(connected) => {
                let was = self.daemon_connected;
                self.daemon_connected = connected;
                if !connected {
                    self.pending_load = None;
                }
                if was && !connected {
                    self.system_msg("Connection to daemon lost — reconnecting…", Effect::None)
                } else if !was && connected {
                    self.system_msg("Reconnected to daemon.", Effect::None)
                } else {
                    vec![Effect::None]
                }
            }
            Action::Tick => vec![Effect::None],
        }
    }

    fn push_tool_history(&mut self, tool_calls: &serde_json::Value) {
        let arr = match tool_calls.as_array() {
            Some(a) => a,
            None => return,
        };
        for call in arr {
            let func = call.get("function");
            let name = func
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("?")
                .to_string();
            let args = func
                .and_then(|f| f.get("arguments"))
                .map(|a| {
                    if let Some(s) = a.as_str() {
                        truncate_for_display(s, TOOL_ARGS_TRUNCATE)
                    } else {
                        a.to_string()
                    }
                })
                .unwrap_or_default();
            self.messages
                .push(Message::ToolCall(ToolCallChip { name, args }));
        }
    }
}

/// Returns the byte index of the start of the word at or before `cursor` in the input.
pub(crate) fn prev_word_boundary(input: &str, cursor: usize) -> usize {
    let bytes = input.as_bytes();
    if cursor == 0 {
        return 0;
    }
    let mut i = cursor;
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    while i > 0 && !bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    i
}

/// Returns the byte index of the end of the word at or after `cursor`.
pub(crate) fn next_word_boundary(input: &str, cursor: usize) -> usize {
    let bytes = input.as_bytes();
    if cursor >= bytes.len() {
        return cursor;
    }
    let mut i = cursor;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

use crate::format::{format_uptime, truncate_for_display};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::relative_time;
    use crossterm::event::{MouseButton, MouseEventKind};

    fn test_app() -> App {
        App::new("http://127.0.0.1:4201".to_string(), ThemeRegistry::new())
    }
    fn conv(id: &str, title: &str) -> ConvHeader {
        ConvHeader {
            id: id.into(),
            title: title.into(),
            created_at: String::new(),
            parent_conversation: None,
            spawned_by: None,
        }
    }
    fn child_conv(id: &str, title: &str, parent: &str) -> ConvHeader {
        ConvHeader {
            id: id.into(),
            title: title.into(),
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
    fn set_layout(app: &mut App, chat: Rect, input: Rect, sidebar_conv: Rect) {
        app.layout.chat = chat;
        app.layout.sidebar_full = sidebar_conv;
        app.layout.input = input;
        app.layout.sidebar_conversations = sidebar_conv;
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
    fn tab_switches_focus_to_sidebar() {
        let mut app = test_app();
        assert_eq!(app.focus, Focus::Input);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::SidebarConversations);
    }
    #[test]
    fn tab_from_sidebar_goes_to_chat() {
        let mut app = test_app();
        app.focus = Focus::SidebarConversations;
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::ChatMessages);
    }
    #[test]
    fn esc_from_sidebar_returns_to_input() {
        let mut app = test_app();
        app.focus = Focus::SidebarConversations;
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.focus, Focus::Input);
    }
    #[test]
    fn sidebar_jk_navigation() {
        let mut app = test_app();
        app.conversations = vec![conv("1", "a"), conv("2", "b")];
        app.focus = Focus::SidebarConversations;
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
        app.focus = Focus::SidebarConversations;
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
        app.focus = Focus::SidebarConversations;
        app.handle_key(key(KeyCode::Char('s')));
        app.handle_key(key(KeyCode::Char('e')));
        assert_eq!(app.sidebar_filter, "se");
    }
    #[test]
    fn backspace_in_sidebar_removes_filter_char() {
        let mut app = test_app();
        app.focus = Focus::SidebarConversations;
        app.sidebar_filter = "ab".into();
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.sidebar_filter, "a");
    }
    #[test]
    fn esc_clears_sidebar_filter_then_returns_to_input() {
        let mut app = test_app();
        app.focus = Focus::SidebarConversations;
        app.sidebar_filter = "search".into();
        app.handle_key(key(KeyCode::Esc));
        assert!(app.sidebar_filter.is_empty());
        assert_eq!(app.focus, Focus::Input);
    }
    #[test]
    fn tab_clears_filter_and_goes_to_chat() {
        let mut app = test_app();
        app.focus = Focus::SidebarConversations;
        app.sidebar_filter = "search".into();
        app.handle_key(key(KeyCode::Tab));
        assert!(app.sidebar_filter.is_empty());
        assert_eq!(app.focus, Focus::ChatMessages);
    }
    #[test]
    fn n_with_filter_appends_not_new_conversation() {
        let mut app = test_app();
        app.focus = Focus::SidebarConversations;
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
    fn slash_model_informational() {
        let mut app = test_app();
        app.input = "/model".into();
        app.handle_key(key(KeyCode::Enter));
        let last = app.messages.last().unwrap();
        assert!(matches!(last, Message::System(m) if m.contains("server-side")));
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
        app.conversations = vec![conv("c1", "a"), conv("c2", "b")];
        app.input = "/session list".into();
        app.handle_key(key(KeyCode::Enter));
        let last = app.messages.last().unwrap();
        assert!(matches!(last, Message::System(m) if m.contains("2 conversations")));
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

    // ── Tree tests ──

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
    fn sidebar_space_toggles_collapse() {
        let mut app = test_app();
        app.conversations = vec![conv("1", "parent"), child_conv("2", "child", "1")];
        app.focus = Focus::SidebarConversations;
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(app.collapsed_nodes.contains("1"));
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(!app.collapsed_nodes.contains("1"));
    }

    #[test]
    fn sidebar_enter_on_leaf_loads_conversation() {
        let mut app = test_app();
        app.conversations = vec![conv("c1", "leaf")];
        app.focus = Focus::SidebarConversations;
        let effects = app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.pending_load, Some("c1".into()));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadConversationHistory(_)))
        );
    }

    #[test]
    fn sidebar_enter_on_parent_toggles_collapse() {
        let mut app = test_app();
        app.conversations = vec![conv("1", "parent"), child_conv("2", "child", "1")];
        app.focus = Focus::SidebarConversations;
        app.handle_key(key(KeyCode::Enter));
        assert!(app.collapsed_nodes.contains("1"));
        app.handle_key(key(KeyCode::Enter));
        assert!(!app.collapsed_nodes.contains("1"));
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

    // ── Mouse tests ──

    #[test]
    fn mouse_click_chat_focuses_chat() {
        let mut app = test_app();
        app.focus = Focus::SidebarConversations;
        app.messages.push(Message::System("hi".into()));
        set_layout(
            &mut app,
            Rect::new(0, 0, 60, 20),
            Rect::new(0, 21, 60, 3),
            Rect::new(61, 0, 20, 20),
        );
        app.handle_mouse(left_click(5, 5));
        assert_eq!(app.focus, Focus::ChatMessages);
    }

    #[test]
    fn mouse_click_input_focuses_input() {
        let mut app = test_app();
        app.focus = Focus::SidebarConversations;
        app.input = "hello".to_string();
        set_layout(
            &mut app,
            Rect::new(0, 0, 60, 20),
            Rect::new(0, 21, 60, 3),
            Rect::new(61, 0, 20, 20),
        );
        app.handle_mouse(left_click(2, 22));
        assert_eq!(app.focus, Focus::Input);
    }

    #[test]
    fn mouse_click_sidebar_selects_item() {
        let mut app = test_app();
        app.conversations = vec![conv("c1", "test"), conv("c2", "other")];
        set_layout(
            &mut app,
            Rect::new(0, 0, 60, 20),
            Rect::new(0, 21, 60, 3),
            Rect::new(61, 0, 20, 20),
        );
        app.handle_mouse(left_click(62, 2));
        assert_eq!(app.focus, Focus::SidebarConversations);
        assert_eq!(app.sidebar_selection, 1);
    }

    #[test]
    fn mouse_click_sidebar_leaf_loads() {
        let mut app = test_app();
        app.conversations = vec![conv("c1", "test")];
        app.focus = Focus::SidebarConversations;
        app.sidebar_selection = 0;
        set_layout(
            &mut app,
            Rect::new(0, 0, 60, 20),
            Rect::new(0, 21, 60, 3),
            Rect::new(61, 0, 20, 20),
        );
        let effects = app.handle_mouse(left_click(62, 1));
        assert_eq!(app.pending_load, Some("c1".into()));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::LoadConversationHistory(_)))
        );
    }

    #[test]
    fn mouse_click_sidebar_parent_toggles() {
        let mut app = test_app();
        app.conversations = vec![conv("1", "parent"), child_conv("2", "child", "1")];
        app.focus = Focus::SidebarConversations;
        app.sidebar_selection = 0;
        set_layout(
            &mut app,
            Rect::new(0, 0, 60, 20),
            Rect::new(0, 21, 60, 3),
            Rect::new(61, 0, 20, 20),
        );
        app.handle_mouse(left_click(62, 1));
        assert!(app.collapsed_nodes.contains("1"));
        app.handle_mouse(left_click(62, 1));
        assert!(!app.collapsed_nodes.contains("1"));
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
        app.focus = Focus::SidebarConversations;
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
        app.focus = Focus::SidebarConversations;
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
        app.focus = Focus::SidebarConversations;
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

    // ── Cursor movement keys ──

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

    // ── Shift+Enter and sidebar edge cases ──

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
        app.focus = Focus::SidebarConversations;
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.sidebar_filter, " ");
        assert_eq!(app.sidebar_selection, 0);
    }
    #[test]
    fn ctrl_s_from_sidebar_stops_streaming() {
        let mut app = test_app();
        app.focus = Focus::SidebarConversations;
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

    // ── Slash command edge cases ──

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
        app.handle_key(key(KeyCode::Enter));
        let last = app.messages.last().unwrap();
        assert!(matches!(last, Message::System(m) if m.contains("Unknown session command")));
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

    // ── State machine edge cases ──

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

    // ── Tree: depth 2+ nesting ──

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

    // ── Utility functions ──

    #[test]
    fn format_uptime_zero() {
        assert_eq!(format_uptime(0), "0m 0s");
    }
    #[test]
    fn format_uptime_one_hour() {
        assert_eq!(format_uptime(3600), "1h 0m");
    }
    #[test]
    fn format_uptime_one_hour_one_minute() {
        assert_eq!(format_uptime(3665), "1h 1m");
    }
    #[test]
    fn format_uptime_seconds_only() {
        assert_eq!(format_uptime(45), "0m 45s");
    }
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
        app.focus = Focus::SidebarConversations;
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
    fn non_alphanumeric_sidebar_typing_ignored() {
        let mut app = test_app();
        app.focus = Focus::SidebarConversations;
        app.sidebar_filter.clear();
        app.handle_key(key(KeyCode::Char('.')));
        app.handle_key(key(KeyCode::Char('@')));
        assert!(app.sidebar_filter.is_empty());
    }
    #[test]
    fn sidebar_up_at_zero_does_nothing() {
        let mut app = test_app();
        app.conversations = vec![conv("c1", "a")];
        app.focus = Focus::SidebarConversations;
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

    // ── Chat focus tests ──

    #[test]
    fn tab_cycles_input_sidebar_chat() {
        let mut app = test_app();
        assert_eq!(app.focus, Focus::Input);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::SidebarConversations);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::ChatMessages);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Input);
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

    // ── Word navigation ──

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

    // ── Coverage gap tests ──

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
        app.focus = Focus::SidebarConversations;
        app.sidebar_filter.clear();
        app.session = Some("old".into());
        app.messages.push(Message::User("hi".into()));
        app.handle_key(key(KeyCode::Char('n')));
        assert!(app.session.is_none());
        assert!(app.messages.is_empty());
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
    fn slash_session_bare_is_list() {
        let mut app = test_app();
        app.input = "/session".into();
        app.handle_key(key(KeyCode::Enter));
        let last = app.messages.last().unwrap();
        assert!(matches!(last, Message::System(m) if m.contains("No active session")));
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
        app.handle_key(key(KeyCode::Enter));
        let last = app.messages.last().unwrap();
        assert!(matches!(last, Message::System(m) if m.contains("Unknown session command")));
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
}
