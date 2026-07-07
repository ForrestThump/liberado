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
        let Some(cmd) = liberado_commands::parse(input) else {
            self.input.clear();
            self.cursor = 0;
            self.input_scroll = 0;
            self.messages.push(Message::System(format!(
                "Unknown command: {input}. Type /help for available commands."
            )));
            self.scroll_offset = 0;
            return vec![Effect::None];
        };

        let is_help = matches!(cmd, liberado_commands::SlashCommand::Help);
        let results = liberado_commands::dispatch(&cmd, self);

        let mut effects = Vec::new();
        for result in &results {
            match result {
                liberado_commands::CommandResult::Quit => effects.push(Effect::Quit),
                liberado_commands::CommandResult::NewConversation { was_streaming } => {
                    effects.push(Effect::RefreshConversations);
                    if *was_streaming {
                        effects.push(Effect::CancelStream);
                    }
                }
                liberado_commands::CommandResult::SessionSwitched { id } => {
                    self.pending_load = Some(id.clone());
                    effects.push(Effect::LoadConversationHistory(id.clone()));
                }
                liberado_commands::CommandResult::ForkRequested { parent_id } => {
                    effects.push(Effect::ForkConversation(parent_id.clone()));
                }
                liberado_commands::CommandResult::ShowOptions { title, options } => {
                    let mut text = format!("{title}:\n");
                    for (label, _id) in options {
                        text.push_str("  ");
                        text.push_str(label);
                        text.push('\n');
                    }
                    self.messages.push(Message::System(text));
                }
                _ => {}
            }
        }
        // The shared help text is client-agnostic; the TUI's keybindings are not, so they're
        // appended here rather than baked into `liberado-commands`.
        if is_help {
            self.messages.push(Message::System(
                "\
Keybindings:
  Enter       send message (Shift+Enter for newline)
  Ctrl+C      clear input, or quit when empty (press twice to exit)
  Ctrl+S      stop streaming (keep partial response)
  Tab         switch focus between input and sidebar
  Esc         clear input / cancel stream / return focus
  PgUp/PgDn   scroll chat
  j / k       navigate sidebar conversations
  Space       toggle tree fold (on sidebar parent nodes)
  n           new conversation (when sidebar focused)
  ← →         move cursor in input
  Home / End  jump to start/end of input
  Del         delete character after cursor"
                    .into(),
            ));
        }
        self.scroll_offset = 0;
        if effects.is_empty() {
            effects.push(Effect::None);
        }
        effects
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

use crate::format::truncate_for_display;
use liberado_commands::format_uptime;

#[cfg(test)]
#[path = "app/tests.rs"]
mod tests;
