//! Application state machine for the Liberado TUI.
//!
//! `App` is the single source of truth for the terminal's state. It is mutated only by
//! `App::update(action) → Vec<Effect>` — the state transition is pure (no I/O), and
//! the returned `Effect` instructions drive any side effects in `main.rs`.
//!
//! The ratatui draw loop reads `App` immutably through a shared lock; actions are
//! produced by background tasks (HTTP poller, SSE stream, keyboard input) and fed
//! through `App::update` one at a time.

use crate::api::{ChatMessage, ConvHeader, DaemonStatus, ReactionEvent};

/// Where keyboard focus sits. Only one area is interactive at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Typing into the chat composer.
    Input,
    /// Navigating the conversation list in the sidebar.
    SidebarConversations,
}

/// A rendered message in the chat pane. Covers user text, assistant text, and
/// inline tool chips.
#[derive(Debug, Clone)]
pub enum Message {
    User(String),
    Assistant(String),
    ToolCall(ToolCallChip),
    ToolResult(ToolResultChip),
}

// Re-export chip types so the enum variants compile.
use crate::api::{ToolCallChip, ToolResultChip};

/// The full terminal state. Held behind `Arc<Mutex<App>>`.
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
    pub should_quit: bool,
}

impl App {
    pub fn new(server: String) -> Self {
        todo!("App::new: initialize default state with empty fields")
    }
}

/// Events that flow into the state machine.
#[derive(Debug, Clone)]
pub enum Action {
    /// A crossterm key event.
    Input(KeyEvent),
    /// `GET /api/status` response arrived.
    StatusUpdate(DaemonStatus),
    /// `GET /api/reactions` response arrived.
    ReactionsUpdate(Vec<ReactionEvent>),
    /// `GET /api/conversations` response arrived.
    ConversationsUpdate(Vec<ConvHeader>),
    /// `GET /api/conversations/{id}` history loaded for resume.
    HistoryLoaded { id: String, messages: Vec<ChatMessage> },
    /// One SSE event from the chat stream.
    SseSession(String),
    SseToken(String),
    SseTool { name: String, args: String },
    SseToolResult { name: String, ok: bool, preview: String },
    SseDone,
    SseFailed(String),
    /// Polling ticker fired — refresh status and reactions.
    Tick,
}

// Re-export crossterm's KeyEvent for action dispatch.
use crossterm::event::KeyEvent;

/// Instructions returned by `App::update()` that `main.rs` executes.
#[derive(Debug)]
pub enum Effect {
    /// Open an SSE chat stream with this message and optional session.
    StartChatStream { message: String, session: Option<String> },
    /// Fetch `/api/conversations` (e.g. after a session ends).
    RefreshConversations,
    /// Fetch `/api/conversations/{id}` to load history before resuming.
    LoadConversationHistory(String),
    /// Quit the TUI.
    Quit,
    /// No side effect — just re-render.
    None,
}

impl App {
    /// The pure state machine. Takes an `Action`, mutates `self`, and returns zero or
    /// more `Effect` instructions for `main` to execute. The caller must not do I/O
    /// inside this function — that's what `Effect` is for.
    pub fn update(&mut self, action: Action) -> Vec<Effect> {
        todo!("App::update: match on Action variant, update App fields, return Effects")
    }

    /// Map an input `KeyEvent` to an `Action`, folding in the current `Focus` so the
    /// same key can mean different things in different contexts.
    pub fn key_action(&self, key: KeyEvent) -> Option<Action> {
        todo!("App::key_action: dispatch on focus + key code")
    }
}
