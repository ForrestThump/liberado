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
    ChatMessage, ConvHeader, DaemonStatus, GoalMessageOutcome, ReactionEvent, SessionKind,
    SessionSummary, ToolCallChip, ToolResultChip,
};
use crate::md_cache::MarkdownParseCache;
use crate::tuning::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The text input area at the bottom.
    Input,
    /// Full-screen searchable prior conversations (`/session`).
    SessionBrowser,
    /// Full-screen unified session switcher (`/sessions`) — primary chat + goal sessions.
    SessionSwitcher,
    /// Full-screen searchable model browser (`/model`).
    ModelBrowser,
    /// Scrollable message history in the chat pane (j/k, Enter expand tools).
    ChatMessages,
}

/// A goal session the UI has moved input focus onto (`/join`). Its transcript is **separate but
/// linked** — a distinct message buffer, fed by the session's own event stream, not mixed into the
/// primary conversation (session-focus D2). When `finished`, the view stays visible (showing the
/// terminal summary) but input routes back to the primary chat; `/back` or the next chat message
/// closes it.
#[derive(Debug, Clone)]
pub struct JoinedSession {
    pub id: String,
    pub kind: SessionKind,
    pub description: String,
    /// Lifecycle tag mirrored from the stream (`"running"`, `"awaiting"`, `"succeeded"`, …).
    pub status: String,
    pub messages: Vec<Message>,
    /// Token accumulator for packs that stream tokens; flushed to an `Assistant` message at the
    /// next structured event or on finish.
    pub stream_buf: String,
    /// Set while the pack is blocked on human input; the input box goes "hot" and shows the prompt.
    pub awaiting: Option<AwaitingPrompt>,
    /// True once a terminal event arrived — input reverts to the primary chat.
    pub finished: bool,
    /// Live gate votes rendered in the goal sidebar — updated as each vote streams in.
    pub gate_votes: Vec<GateVote>,
    /// The currently active role, if any. `None` between roles.
    pub active_role: Option<String>,
    /// The most recent validation result, if any.
    pub last_validation: Option<ValidationResult>,
}

/// One completion-gate vote, rendered in the goal sidebar.
#[derive(Debug, Clone)]
pub struct GateVote {
    pub reviewer: String,
    pub kind: String,
    pub approved: bool,
    pub coerced: bool,
}

/// The most recent validation result for the goal sidebar.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub ok: bool,
    pub summary: String,
}

/// The prompt an interactive session is blocked on (`AwaitingInput`).
#[derive(Debug, Clone)]
pub struct AwaitingPrompt {
    pub prompt: String,
    pub options: Vec<String>,
}

/// A goal-session stream frame mapped to what the joined view needs to render — a purpose-built
/// projection of `SessionEventKind` (mapping lives in [`crate::sse`], close to the decoder).
#[derive(Debug, Clone)]
pub enum GoalUiEvent {
    Started {
        description: String,
    },
    Token(String),
    ToolStarted {
        name: String,
        args: String,
    },
    ToolFinished {
        name: String,
        ok: bool,
        preview: String,
    },
    Role {
        role: String,
        model: Option<String>,
    },
    Progress(String),
    Validation {
        ok: bool,
        summary: String,
    },
    /// One completion-gate reviewer vote. `coerced` = the gate substituted a refusal because the
    /// reviewer was unavailable, which reads very differently to a human than a real rejection.
    CriticVerdict {
        reviewer: String,
        kind: String,
        approved: bool,
        issues: Vec<String>,
        coerced: bool,
    },
    /// A workspace file changed. Accumulated into the session's changed-file list.
    FileChanged {
        path: String,
        change: String,
    },
    LoopGuard(String),
    Awaiting {
        prompt: String,
        options: Vec<String>,
    },
    Human(String),
    Finished {
        status: String,
        summary: String,
    },
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
    /// Where `set_theme` persists the theme preference. `Some` in the real binary (the platform
    /// config `settings.toml`); `None` in tests so they never touch — or clobber — the user's config.
    pub settings_path: Option<std::path::PathBuf>,
    pub daemon_connected: bool,
    pub collapsed_nodes: HashSet<String>,
    pub expanded_messages: HashSet<usize>,
    pub chat_cursor: usize,
    /// How many of the human's turns were pruned off the top of `messages` on history load.
    ///
    /// Turn numbers are rendered beside user messages so `/fork <n>` is something you can *point*
    /// at. The server counts turns from the real start of the conversation, so if the client has
    /// dropped the first N turns to stay under `MAX_MESSAGE_COUNT` and then numbers what's left from
    /// 1, every number on screen is wrong — and `/fork 3` would silently branch at a different place
    /// than the one you clicked. This offset is what keeps the two counting the same thing.
    pub turn_offset: usize,
    pub input_max_height: u16,
    pub input_scroll: usize,
    pub layout: LayoutRects,
    /// Content-keyed parse cache for assistant markdown (T1.1 — avoid reparse every frame).
    pub md_cache: MarkdownParseCache,
    /// T1.3: when false, the main loop skips `terminal.draw` (idle CPU near zero).
    /// Set by input handlers and state-changing actions; cleared after a successful paint.
    dirty: bool,
    /// Selected row in the slash-command palette (when input starts with `/`).
    pub slash_palette_index: usize,
    /// Live model ids from `GET /api/models` (shown in ModelBrowser).
    pub models: Vec<String>,
    /// Soft error from last models fetch.
    pub models_error: Option<String>,
    /// True while a models fetch is in flight.
    pub models_loading: bool,
    /// **Every** session from `GET /api/sessions` (S5′) — chats *and* goal sessions, one list.
    /// This is the switcher's only source; the client no longer stitches two endpoints together.
    pub sessions: Vec<SessionSummary>,
    /// The goal session input focus is currently on (`/join`), if any. `None` = the primary chat.
    pub joined: Option<JoinedSession>,
}

/// Layout rectangles populated by the draw pass for mouse hit-testing.
#[derive(Debug, Clone, Default)]
pub struct LayoutRects {
    pub status_bar: Rect,
    pub chat: Rect,
    pub input: Rect,
    /// Full-screen area used while the session browser is open.
    pub session_browser: Rect,
    /// Goal sidebar (right of chat) — rendered when a goal session is joined.
    pub goal_sidebar: Rect,
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
        // Preferred theme from `settings.toml` (platform config dir); fall back to built-in dark.
        let preferred = liberado_theme::load_ui_settings()
            .theme
            .filter(|n| registry.get(n).is_some());
        let theme = preferred
            .as_deref()
            .and_then(|n| registry.get(n).cloned())
            .or_else(|| registry.get("dark").cloned())
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
            settings_path: liberado_theme::user_settings_path(),
            daemon_connected: false,
            collapsed_nodes: HashSet::new(),
            expanded_messages: HashSet::new(),
            chat_cursor: 0,
            turn_offset: 0,
            input_max_height: INPUT_MAX_HEIGHT,
            input_scroll: 0,
            layout: LayoutRects::default(),
            md_cache: MarkdownParseCache::new(),
            dirty: true, // first paint
            slash_palette_index: 0,
            models: Vec::new(),
            models_error: None,
            models_loading: false,
            sessions: Vec::new(),
            joined: None,
        }
    }

    /// Progressive slash matches for the current input (shared catalog).
    pub fn slash_matches(&self) -> Vec<&'static liberado_commands::CommandSpec> {
        liberado_commands::filter_commands(&self.input)
    }

    /// Dim ghost remainder of the selected slash match (inline after typed text).
    pub fn slash_ghost_suffix(&self) -> Option<String> {
        liberado_commands::ghost_suffix(&self.input, self.slash_palette_index)
    }

    /// Clamp palette cursor after the filter set changes.
    pub fn clamp_slash_palette(&mut self) {
        let n = self.slash_matches().len();
        if n == 0 {
            self.slash_palette_index = 0;
        } else {
            self.slash_palette_index = self.slash_palette_index.min(n - 1);
        }
    }

    /// Enter full-screen searchable session browser (`/session`).
    pub fn open_session_browser(&mut self) {
        self.focus = Focus::SessionBrowser;
        self.sidebar_filter.clear();
        self.sidebar_selection = 0;
        self.input.clear();
        self.cursor = 0;
        self.input_scroll = 0;
        self.mark_dirty();
    }

    /// Leave session browser and return to the chat input.
    pub fn close_session_browser(&mut self) {
        self.focus = Focus::Input;
        self.sidebar_filter.clear();
        self.sidebar_selection = 0;
        self.mark_dirty();
    }

    /// Enter full-screen searchable model browser (`/model`).
    pub fn open_model_browser(&mut self) {
        self.focus = Focus::ModelBrowser;
        self.sidebar_filter.clear();
        self.sidebar_selection = 0;
        self.input.clear();
        self.cursor = 0;
        self.input_scroll = 0;
        self.models_loading = true;
        self.models_error = None;
        self.mark_dirty();
    }

    /// Leave model browser and return to the chat input.
    pub fn close_model_browser(&mut self) {
        self.focus = Focus::Input;
        self.sidebar_filter.clear();
        self.sidebar_selection = 0;
        self.models_loading = false;
        self.mark_dirty();
    }

    /// Models matching the current filter (case-insensitive substring).
    pub fn filtered_models(&self) -> Vec<&String> {
        let q = self.sidebar_filter.to_ascii_lowercase();
        self.models
            .iter()
            .filter(|m| q.is_empty() || m.to_ascii_lowercase().contains(&q))
            .collect()
    }

    pub fn clamp_model_selection(&mut self) {
        let n = self.filtered_models().len();
        if n == 0 {
            self.sidebar_selection = 0;
        } else {
            self.sidebar_selection = self.sidebar_selection.min(n - 1);
        }
    }

    // ── Unified session switcher + goal-session focus (`/sessions`, `/join`, `/back`) ──

    /// Enter the full-screen unified session switcher (`/sessions`).
    pub fn open_session_switcher(&mut self) {
        self.focus = Focus::SessionSwitcher;
        self.sidebar_filter.clear();
        self.sidebar_selection = 0;
        self.input.clear();
        self.cursor = 0;
        self.input_scroll = 0;
        self.mark_dirty();
    }

    /// Leave the switcher, back to the input.
    pub fn close_session_switcher(&mut self) {
        self.focus = Focus::Input;
        self.sidebar_filter.clear();
        self.sidebar_selection = 0;
        self.mark_dirty();
    }

    /// Sessions matching the switcher filter — **one** list (S5′).
    ///
    /// This used to be two: `/api/conversations` for chats and `/api/goals` for goal sessions,
    /// stitched together here. The client was re-deriving a distinction the model says does not
    /// exist. Now `GET /api/sessions` returns both, and the only difference between a row that
    /// *joins* and a row that *opens* is whether it has a goal.
    /// The `after_turn` that forks **at the message under the chat cursor** — i.e. keep the context
    /// that existed *above* it, and leave it and everything below behind.
    ///
    /// `None` means there is nothing above the selection to keep (you are sitting on your very first
    /// turn), which is a fresh conversation, not a fork.
    ///
    /// One count serves both readings of "fork here", because they turn out to be the same number —
    /// the turns that *completed* before the selected message:
    ///
    /// * On **one of your turns**: branches *before* it, so you get back the context you had when you
    ///   typed it and can say something else. (Selecting the message you want to redo is the whole
    ///   point of forking from history.)
    /// * On an **assistant reply or tool chip**: branches *after* the turn it belongs to, so you keep
    ///   that answer and continue a different way from there.
    ///
    /// Turns are counted, not node ids, because a live-streamed message never receives a node id from
    /// the SSE stream — half the messages on screen would be unforkable if the branch point had to be
    /// a node. `turn_offset` keeps this agreeing with the server when a long history has been pruned.
    pub fn fork_turn_at_cursor(&self) -> Option<u32> {
        if self.chat_cursor >= self.messages.len() {
            return None;
        }
        // Both readings collapse to one number: how many of your turns *completed* strictly above
        // the selected message. On your own turn N that is N-1 (branch before it); on the reply to
        // turn N it is N (branch after it, keeping the answer). Same count, both intents.
        let completed = self.messages[..self.chat_cursor]
            .iter()
            .filter(|m| matches!(m, Message::User(_)))
            .count();
        let after_turn = self.turn_offset + completed;
        (after_turn > 0).then_some(after_turn as u32)
    }

    pub fn filtered_sessions(&self) -> Vec<&SessionSummary> {
        let q = self.sidebar_filter.to_ascii_lowercase();
        self.sessions
            .iter()
            .filter(|h| {
                q.is_empty()
                    || h.label().to_ascii_lowercase().contains(&q)
                    || h.kind().label().to_ascii_lowercase().contains(&q)
                    || h.id.to_ascii_lowercase().contains(&q)
            })
            .collect()
    }

    /// Total rows in the unified switcher — one flat list of every session.
    pub fn switcher_row_count(&self) -> usize {
        self.filtered_sessions().len()
    }

    pub fn clamp_switcher_selection(&mut self) {
        let n = self.switcher_row_count();
        if self.sidebar_selection >= n {
            self.sidebar_selection = n.saturating_sub(1);
        }
    }

    /// The kind of the session input is currently focused on — Primary unless joined to a live goal
    /// session (a finished joined session has already handed input back to the chat).
    pub fn current_kind(&self) -> SessionKind {
        match &self.joined {
            Some(j) if !j.finished => j.kind,
            _ => SessionKind::Primary,
        }
    }

    /// The id of the live goal session input should route to, or `None` for the primary chat.
    pub fn input_target_session(&self) -> Option<String> {
        match &self.joined {
            Some(j) if !j.finished => Some(j.id.clone()),
            _ => None,
        }
    }

    /// Begin focusing a goal session: seed a [`JoinedSession`] (kind/description from the switcher
    /// list when known; the stream fills in the rest) and return to the input. The caller pairs
    /// this with an [`Effect::JoinGoalSession`] that opens the SSE stream.
    pub fn join_session(&mut self, id: String) {
        self.join_session_with(id, None, None);
    }

    /// Like [`join_session`](Self::join_session) but with caller-supplied kind/description hints —
    /// used by `/spawn`, where the session isn't in the switcher list yet (it was just created).
    pub fn join_session_with(
        &mut self,
        id: String,
        kind_hint: Option<SessionKind>,
        description_hint: Option<String>,
    ) {
        let header = self.sessions.iter().find(|h| h.id == id);
        let kind = kind_hint
            .or_else(|| header.map(|h| h.kind()))
            .unwrap_or(SessionKind::Custom);
        let description = description_hint
            .or_else(|| header.map(|h| h.description().to_string()))
            .unwrap_or_default();
        let status = header
            .map(|h| h.status.clone())
            .unwrap_or_else(|| "running".to_string());
        self.joined = Some(JoinedSession {
            id,
            kind,
            description,
            status,
            messages: Vec::new(),
            stream_buf: String::new(),
            awaiting: None,
            finished: false,
            gate_votes: Vec::new(),
            active_role: None,
            last_validation: None,
        });
        self.focus = Focus::Input;
        self.scroll_offset = 0;
        self.mark_dirty();
    }

    /// Leave the joined session (`/back`), returning input focus to the primary chat.
    pub fn leave_session(&mut self) {
        self.joined = None;
        self.focus = Focus::Input;
        self.scroll_offset = 0;
        self.mark_dirty();
    }

    /// Flush any buffered stream tokens into an assistant message on the joined transcript.
    fn flush_joined_buf(j: &mut JoinedSession) {
        if !j.stream_buf.is_empty() {
            j.messages
                .push(Message::Assistant(std::mem::take(&mut j.stream_buf)));
        }
    }

    /// Apply one goal-session stream frame to the joined view. A no-op if not joined (stray events
    /// after `/back`) or for a different session id.
    fn apply_goal_event(&mut self, ev: GoalUiEvent) {
        let Some(j) = self.joined.as_mut() else {
            return;
        };
        match ev {
            GoalUiEvent::Started { description } => {
                if !description.is_empty() {
                    j.description = description;
                }
            }
            GoalUiEvent::Token(t) => j.stream_buf.push_str(&t),
            GoalUiEvent::ToolStarted { name, args } => {
                Self::flush_joined_buf(j);
                j.messages
                    .push(Message::ToolCall(ToolCallChip { name, args }));
            }
            GoalUiEvent::ToolFinished { name, ok, preview } => {
                j.messages
                    .push(Message::ToolResult(ToolResultChip { name, ok, preview }));
            }
            GoalUiEvent::Role { role, model } => {
                Self::flush_joined_buf(j);
                j.active_role = Some(role.clone());
                j.last_validation = None;
                let line = match model {
                    Some(m) => format!("▸ {role} · {m}"),
                    None => format!("▸ {role}"),
                };
                j.messages.push(Message::System(line));
            }
            GoalUiEvent::Progress(m) => {
                Self::flush_joined_buf(j);
                j.messages.push(Message::System(format!("… {m}")));
            }
            GoalUiEvent::Validation { ok, summary } => {
                Self::flush_joined_buf(j);
                j.last_validation = Some(ValidationResult {
                    ok,
                    summary: summary.clone(),
                });
                let mark = if ok { "✓" } else { "✗" };
                j.messages
                    .push(Message::System(format!("{mark} {summary}")));
            }
            GoalUiEvent::CriticVerdict {
                reviewer,
                kind,
                approved,
                issues,
                coerced,
            } => {
                Self::flush_joined_buf(j);
                j.gate_votes.push(GateVote {
                    reviewer: reviewer.clone(),
                    kind: kind.clone(),
                    approved,
                    coerced,
                });
                // Votes arrive on a stream that never ends until the goal does, so prune here
                // rather than at a load boundary the way `messages` does.
                if j.gate_votes.len() > MAX_GATE_VOTES {
                    let removed = j.gate_votes.len() - MAX_GATE_VOTES;
                    j.gate_votes.drain(..removed);
                }
                // "?" rather than "✗" when coerced: the reviewer produced no opinion, and showing
                // that as a rejection would make an outage look like the work being wrong.
                let mark = if coerced {
                    "?"
                } else if approved {
                    "✓"
                } else {
                    "✗"
                };
                let detail = if coerced {
                    " unavailable → counted as refuting".to_string()
                } else if issues.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", issues.join("; "))
                };
                j.messages.push(Message::System(format!(
                    "{mark} gate[{kind}] {reviewer}{detail}"
                )));
            }
            GoalUiEvent::FileChanged { path, change } => {
                Self::flush_joined_buf(j);
                let mark = match change.as_str() {
                    "added" => "+",
                    "deleted" => "-",
                    _ => "~",
                };
                j.messages.push(Message::System(format!("{mark} {path}")));
            }
            GoalUiEvent::LoopGuard(m) => {
                Self::flush_joined_buf(j);
                j.messages.push(Message::System(format!("loop-guard: {m}")));
            }
            GoalUiEvent::Awaiting { prompt, options } => {
                Self::flush_joined_buf(j);
                j.status = "awaiting".into();
                j.awaiting = Some(AwaitingPrompt {
                    prompt: prompt.clone(),
                    options,
                });
            }
            GoalUiEvent::Human(text) => {
                Self::flush_joined_buf(j);
                j.awaiting = None;
                if j.status == "awaiting" {
                    j.status = "running".into();
                }
                j.messages.push(Message::User(text));
            }
            GoalUiEvent::Finished { status, summary } => {
                Self::flush_joined_buf(j);
                j.awaiting = None;
                j.active_role = None;
                j.finished = true;
                j.status = status.clone();
                j.messages.push(Message::System(format!(
                    "[session {status}] {summary}  —  /back to return to chat"
                )));
            }
        }
        self.scroll_offset = 0;
        self.mark_dirty();
    }

    /// Mark the UI as needing a redraw (input, resize, meaningful state change).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// True when spinners / streaming buffers should animate (redraw every poll tick).
    pub fn needs_animation(&self) -> bool {
        self.streaming || self.pending_load.is_some() || !self.daemon_connected
    }

    /// Whether the main loop should call `terminal.draw` this iteration.
    pub fn should_draw(&self) -> bool {
        self.dirty || self.needs_animation()
    }

    /// Clear the dirty flag after a successful paint.
    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    #[cfg(test)]
    pub(crate) fn is_dirty(&self) -> bool {
        self.dirty
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
            visual_line += if chars == 0 { 1 } else { chars.div_ceil(cw) };
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
            let visual_lines_in_logical = if chars_in_logical == 0 {
                1
            } else {
                chars_in_logical.div_ceil(cw)
            };
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
                if chars == 0 { 1 } else { chars.div_ceil(cw) }
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
        self.input_scroll = self
            .input_scroll
            .min(total_lines.saturating_sub(max_visible));
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        self.mark_dirty();
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
            Focus::SessionBrowser => crate::handlers::sidebar::handle(self, key),
            Focus::SessionSwitcher => crate::handlers::switcher::handle(self, key),
            Focus::ModelBrowser => crate::handlers::models::handle(self, key),
            Focus::ChatMessages => crate::handlers::chat::handle(self, key),
        }
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) -> Vec<Effect> {
        self.mark_dirty();
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
        // Capture before dispatch: `/new` clears the active session inside liberado-commands
        // (`set_active_session(None)`), so reading `self.session` afterward would drop the cancel
        // target and leave a durable turn running.
        let session_before_dispatch = self.session.clone();
        let results = liberado_commands::dispatch(&cmd, self);

        let mut effects = Vec::new();
        for result in &results {
            match result {
                liberado_commands::CommandResult::Quit => effects.push(Effect::Quit),
                liberado_commands::CommandResult::NewConversation { was_streaming } => {
                    effects.push(Effect::RefreshConversations);
                    if *was_streaming {
                        effects.push(Effect::CancelStream {
                            conversation: session_before_dispatch.clone(),
                        });
                    }
                }
                liberado_commands::CommandResult::SessionSwitched { id } => {
                    self.pending_load = Some(id.clone());
                    self.focus = Focus::Input;
                    effects.push(Effect::LoadConversationHistory(id.clone()));
                }
                liberado_commands::CommandResult::OpenSessionBrowser
                | liberado_commands::CommandResult::SessionListed => {
                    // Both `/session` and `/sessions` land on the one unified switcher.
                    self.open_session_switcher();
                    effects.push(Effect::RefreshConversations);
                    effects.push(Effect::RefreshSessions);
                }
                liberado_commands::CommandResult::OpenModelBrowser => {
                    self.open_model_browser();
                    effects.push(Effect::FetchModels);
                }
                liberado_commands::CommandResult::OpenGoalSwitcher => {
                    self.open_session_switcher();
                    effects.push(Effect::RefreshConversations);
                    effects.push(Effect::RefreshSessions);
                }
                liberado_commands::CommandResult::JoinGoalSession { id } => {
                    // Resolve an id prefix against the known goal sessions (full id wins).
                    let resolved = self
                        .sessions
                        .iter()
                        .find(|h| h.id == *id)
                        .or_else(|| self.sessions.iter().find(|h| h.id.starts_with(id)))
                        .map(|h| h.id.clone())
                        .unwrap_or_else(|| id.clone());
                    self.join_session(resolved.clone());
                    effects.push(Effect::JoinGoalSession(resolved));
                }
                liberado_commands::CommandResult::BackToPrimary => {
                    if self.joined.is_some() {
                        self.leave_session();
                        effects.push(Effect::LeaveGoalSession);
                    } else {
                        self.messages
                            .push(Message::System("Already in the primary chat.".into()));
                    }
                }
                liberado_commands::CommandResult::SpawnGoalSession { domain, goal } => {
                    // Leaving a finished joined view (if any) so the new session takes the pane.
                    if self.joined.as_ref().map(|j| j.finished).unwrap_or(false) {
                        self.joined = None;
                    }
                    effects.push(Effect::SpawnGoalSession {
                        domain: domain.clone(),
                        goal: goal.clone(),
                        // Link the new session back to the current conversation so its summary folds
                        // in on terminal (S4 return handoff). `None` when there's no chat yet.
                        origin_conversation: self.session.clone(),
                    });
                }
                liberado_commands::CommandResult::ForkRequested {
                    parent_id,
                    after_turn,
                } => {
                    effects.push(Effect::ForkConversation {
                        parent_id: parent_id.clone(),
                        after_turn: *after_turn,
                    });
                }
                liberado_commands::CommandResult::StartCodingGoal {
                    project,
                    text,
                    mode,
                } => {
                    if self.joined.as_ref().map(|j| j.finished).unwrap_or(false) {
                        self.joined = None;
                    }
                    effects.push(Effect::StartCodingGoal {
                        project: project.clone(),
                        text: text.clone(),
                        mode: *mode,
                        origin_conversation: self.session.clone(),
                    });
                }
                liberado_commands::CommandResult::OpenGoalView => match &self.joined {
                    Some(j) => {
                        let id = j.id.clone();
                        self.messages.push(Message::System(format!(
                            "Goal view: {id} (you are already joined — the pane below is the view)"
                        )));
                    }
                    None => self.messages.push(Message::System(
                        "No session focused. Use /goal <text> to start one, or /sessions to join."
                            .into(),
                    )),
                },
                liberado_commands::CommandResult::GoalStatus => match &self.joined {
                    Some(j) => {
                        let line = format!(
                            "{} · {} · {} message(s){}",
                            j.id,
                            j.status,
                            j.messages.len(),
                            match &j.awaiting {
                                Some(a) => format!(" · awaiting: {}", a.prompt),
                                None => String::new(),
                            }
                        );
                        self.messages.push(Message::System(line));
                    }
                    None => self
                        .messages
                        .push(Message::System("No goal session focused.".into())),
                },
                liberado_commands::CommandResult::ParkGoalSession => {
                    match self.joined.as_ref().map(|j| j.id.clone()) {
                        Some(id) => effects.push(Effect::ParkGoalSession(id)),
                        None => self
                            .messages
                            .push(Message::System("No goal session to park.".into())),
                    }
                }
                liberado_commands::CommandResult::ResumeGoalSession { answer } => {
                    match self.joined.as_ref().map(|j| j.id.clone()) {
                        Some(id) => effects.push(Effect::ResumeGoalSession {
                            id,
                            answer: answer.clone(),
                        }),
                        None => self
                            .messages
                            .push(Message::System("No goal session to resume.".into())),
                    }
                }
                liberado_commands::CommandResult::CancelGoalSession => {
                    match self.joined.as_ref().map(|j| j.id.clone()) {
                        Some(id) => effects.push(Effect::CancelGoalSession(id)),
                        None => self
                            .messages
                            .push(Message::System("No goal session to cancel.".into())),
                    }
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
  Enter       send / accept slash ghost · expand tool in chat
  Ctrl+C      clear input, or quit when empty (press twice to exit)
  Ctrl+S      stop / cancel the in-flight turn (nothing is kept — cancel discards)
  Tab         input ↔ chat history (or slash complete)
  Esc         clear input / cancel stream / leave a browser
  /sessions   switch sessions (primary chat + goal sessions)
  /spawn d g  start an interactive session: /spawn <domain> <goal>
  /join <id>  focus a goal session · /back returns to the primary chat
  /session    full-screen searchable prior conversations
  /model      full-screen searchable model browser
  j / k       navigate chat or a browser
  PgUp/PgDn   scroll chat
  ← →         move cursor in input
  Home / End  jump to start/end of input"
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

    /// Stop watching and cancel the durable turn on the daemon.
    ///
    /// Pre-durable-turns, closing the SSE stream was enough to cancel. Turns now outlive the
    /// connection, so local teardown alone only hides the work while it keeps running and
    /// billing. [`Effect::CancelStream`] posts `POST /api/conversations/{id}/cancel` (and aborts
    /// the local reader). The daemon persists **nothing** for a cancelled turn — drop any partial
    /// assistant buffer rather than pretending a reply was kept.
    fn end_stream(&mut self, label: &str) -> Vec<Effect> {
        self.streaming = false;
        self.assistant_buf.clear();
        self.messages.push(Message::System(label.into()));
        self.scroll_offset = 0;
        vec![Effect::CancelStream {
            conversation: self.session.clone(),
        }]
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
    /// Model catalog from `GET /api/models`.
    ModelsLoaded {
        models: Vec<String>,
        error: Option<String>,
    },
    /// Result of `POST /api/models/select`.
    ModelSelected {
        model: String,
        error: Option<String>,
        /// Whether the pick was scoped to one conversation (`true`) or daemon-wide (`false`).
        conversation_scoped: bool,
    },
    /// Full message history loaded for a conversation, plus turn lifecycle flags from the same
    /// response (`turn_running` / `turn_unanswered`).
    HistoryLoaded {
        id: String,
        messages: Vec<ChatMessage>,
        turn_running: bool,
        turn_unanswered: bool,
    },
    /// Re-read a conversation's transcript because the reply landed while nobody was attached.
    /// Raised when attach returns `409` — the turn finished between reading `turn_running` and
    /// the attach request, so the answer is on disk and the displayed history is one turn stale.
    ReloadConversationHistory(String),
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
    /// A fork landed: the branch exists on the server and is now where the human is typing.
    Forked(crate::api::ForkResponse),
    /// Goal sessions from `GET /api/goals` — the goal rows of the session switcher.
    SessionsUpdate(Vec<SessionSummary>),
    /// One decoded frame of a joined goal session's event stream.
    GoalStreamEvent(GoalUiEvent),
    /// The joined session's SSE stream ended or errored (connection-level, not a session finish).
    GoalStreamClosed(Option<String>),
    /// Result of `POST /api/goals/{id}/message` — so a 404/409 surfaces in the joined view.
    GoalMessageOutcome(GoalMessageOutcome),
    /// A chat turn offered an interactive specialist session (`session_offered` on the chat stream).
    /// Rendered as a joinable affordance; the human accepts with `/join <id>` (or declines by
    /// ignoring it — the generalist keeps running).
    GoalOffered {
        session_id: String,
        domain: String,
        description: String,
    },
    /// `/spawn` succeeded — a new interactive session was created; focus it.
    GoalSpawned {
        session_id: String,
        domain: String,
        description: String,
    },
    /// `/spawn` failed (couldn't start the session).
    GoalSpawnFailed(String),
    /// A background effect finished and has something to tell the human (goal park/cancel results).
    /// Routed as an action rather than written directly so all scrollback mutation stays on the
    /// reducer, which is what the reducer tests exercise.
    SystemMessage(String),
    /// Daemon connectivity transition (true = connected, false = lost).
    ConnectionStatus(bool),
    /// Periodic heartbeat from the poller (currently a no-op in `update()`).
    Tick,
}

/// Commands returned by [`App`] for the main loop / `EffectRunner` to execute.
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
    /// Fetch `GET /api/models` for the model browser.
    FetchModels,
    /// Hot-swap model (`POST /api/models/select`). `conversation` scopes to one chat when set.
    SelectModel {
        model: String,
        conversation: Option<String>,
    },
    /// Abort local SSE reader and, when `conversation` is set, `POST …/cancel` on the daemon
    /// (durable turns outlive the connection — local teardown alone is not enough).
    CancelStream {
        conversation: Option<String>,
    },
    /// Rejoin a turn already in flight (`GET /api/conversations/{id}/attach`).
    AttachConversationStream(String),
    /// Branch a conversation, keeping the original (`POST /api/sessions/{id}/fork`).
    ForkConversation {
        parent_id: String,
        /// Keep through this turn of the human's (1-based). `None` = the whole conversation.
        after_turn: Option<u32>,
    },
    SetWindowTitle(String),
    /// Fetch `GET /api/goals` to populate the session switcher.
    RefreshSessions,
    /// Open the joined goal session's SSE stream (`GET /api/goals/{id}/stream`).
    JoinGoalSession(String),
    /// Deliver a human message into the joined session (`POST /api/goals/{id}/message`).
    SendGoalMessage {
        id: String,
        text: String,
    },
    /// Start a new interactive session (`/spawn`): `POST /api/goals` then focus it.
    SpawnGoalSession {
        domain: String,
        goal: String,
        origin_conversation: Option<String>,
    },
    /// Start a **coding** goal (`/goal` or `/explore`): `POST /api/goals` with domain `coding`.
    StartCodingGoal {
        project: Option<String>,
        text: String,
        /// Restricted tier for this goal: plan (writes only the plan file) or explore
        /// (no writes at all). `None` is a normal full-write coding goal.
        mode: Option<liberado_commands::CodingGoalMode>,
        origin_conversation: Option<String>,
    },
    /// Park the joined session (`POST /api/goals/{id}/park`) — graceful, resumable.
    ParkGoalSession(String),
    /// Resume a parked session (`POST /api/goals/{id}/message`), optionally answering it.
    ResumeGoalSession {
        id: String,
        answer: String,
    },
    /// Cancel the joined session (`POST /api/goals/{id}/cancel`) — terminal.
    CancelGoalSession(String),
    /// Abort the joined session's SSE stream (on `/back`).
    LeaveGoalSession,
    Quit,
    None,
}

impl App {
    pub fn update(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::StatusUpdate(status) => {
                if self.status.as_ref() != Some(&status) {
                    self.status = Some(status);
                    self.mark_dirty();
                }
                vec![Effect::None]
            }
            Action::ReactionsUpdate(reactions) => {
                if self.reactions != reactions {
                    self.reactions = reactions;
                    self.mark_dirty();
                }
                vec![Effect::None]
            }
            Action::ConversationsUpdate(convs) => {
                if self.conversations != convs {
                    self.conversations = convs;
                    if self.sidebar_selection >= self.conversations.len().saturating_sub(1) {
                        self.sidebar_selection = self.conversations.len().saturating_sub(1);
                    }
                    self.mark_dirty();
                }
                vec![Effect::None]
            }
            Action::ModelsLoaded { models, error } => {
                self.models = models;
                self.models_error = error;
                self.models_loading = false;
                self.clamp_model_selection();
                self.mark_dirty();
                vec![Effect::None]
            }
            Action::ModelSelected {
                model,
                error,
                conversation_scoped,
            } => self.on_model_selected(model, error, conversation_scoped),
            Action::Forked(fork) => {
                // Land in the branch. The original is untouched and still in the switcher — say so,
                // because "fork" sounds like it might have moved or rewritten the conversation, and
                // the whole value of copy semantics is that it did neither.
                let kept = if fork.kept_turns == fork.total_turns {
                    format!("all {} turns", fork.total_turns)
                } else {
                    format!("turns 1–{} of {}", fork.kept_turns, fork.total_turns)
                };
                self.messages.push(Message::System(format!(
                    "Forked into a new conversation with {kept}. You're now in the branch — the \
                     original is untouched, in /sessions."
                )));
                // `pending_load` is what `HistoryLoaded` checks against, so set it before asking:
                // otherwise the load comes back and is discarded as stale.
                self.pending_load = Some(fork.id.clone());
                self.mark_dirty();
                vec![
                    Effect::LoadConversationHistory(fork.id.clone()),
                    Effect::RefreshSessions,
                ]
            }
            Action::HistoryLoaded {
                id,
                messages,
                turn_running,
                turn_unanswered,
            } => self.on_history_loaded(id, messages, turn_running, turn_unanswered),
            Action::ReloadConversationHistory(id) => {
                // Same handshake every other open path uses: `HistoryLoaded` discards a response
                // whose id is not the pending one, so claim it before asking.
                self.pending_load = Some(id.clone());
                self.mark_dirty();
                vec![Effect::LoadConversationHistory(id)]
            }
            Action::SseSession(id) => {
                if self.session.is_none() {
                    self.session = Some(id);
                    self.mark_dirty();
                }
                vec![Effect::None]
            }
            Action::SseToken(token) => {
                self.assistant_buf.push_str(&token);
                self.mark_dirty();
                vec![Effect::None]
            }
            Action::SseTool { name, args } => {
                self.messages
                    .push(Message::ToolCall(ToolCallChip { name, args }));
                self.mark_dirty();
                vec![Effect::None]
            }
            Action::SseToolResult { name, ok, preview } => {
                self.messages
                    .push(Message::ToolResult(ToolResultChip { name, ok, preview }));
                self.mark_dirty();
                vec![Effect::None]
            }
            Action::SseDone => {
                let finished = std::mem::take(&mut self.assistant_buf);
                if !finished.is_empty() {
                    self.messages.push(Message::Assistant(finished));
                }
                self.streaming = false;
                self.scroll_offset = 0;
                self.mark_dirty();
                vec![Effect::RefreshConversations]
            }
            Action::SseFailed(err) => {
                self.pending_load = None;
                self.assistant_buf.clear();
                self.messages
                    .push(Message::System(format!("[error] {err}")));
                self.streaming = false;
                self.mark_dirty();
                vec![Effect::RefreshConversations]
            }
            Action::SessionsUpdate(sessions) => {
                self.sessions = sessions;
                self.clamp_switcher_selection();
                self.mark_dirty();
                vec![Effect::None]
            }
            Action::GoalStreamEvent(ev) => {
                self.apply_goal_event(ev);
                vec![Effect::None]
            }
            Action::GoalOffered {
                session_id,
                domain,
                description,
            } => {
                // Render the offer inline in the chat as a joinable affordance. The human accepts by
                // running `/join <id>`; ignoring it just leaves the generalist running (D3 consent).
                let kind = kind_from_domain_str(&domain);
                self.messages.push(Message::System(format!(
                    "▸ {} session offered: {}\n  /join {}  to focus it   (or keep chatting here)",
                    kind.label(),
                    if description.is_empty() {
                        session_id.as_str()
                    } else {
                        description.as_str()
                    },
                    session_id
                )));
                self.scroll_offset = 0;
                self.mark_dirty();
                vec![Effect::None]
            }
            Action::GoalSpawned {
                session_id,
                domain,
                description,
            } => {
                // A `/spawn` session was created — focus it immediately (the human asked for it).
                let kind = kind_from_domain_str(&domain);
                self.join_session_with(session_id.clone(), Some(kind), Some(description));
                vec![Effect::JoinGoalSession(session_id)]
            }
            Action::SystemMessage(text) => {
                self.messages.push(Message::System(text));
                self.scroll_offset = 0;
                self.mark_dirty();
                vec![Effect::None]
            }
            Action::GoalSpawnFailed(err) => {
                self.messages
                    .push(Message::System(format!("[spawn failed] {err}")));
                self.scroll_offset = 0;
                self.mark_dirty();
                vec![Effect::None]
            }
            Action::GoalStreamClosed(err) => {
                if let Some(j) = self.joined.as_mut()
                    && !j.finished
                {
                    // A connection-level end without a session-finish event: note it, but keep the
                    // transcript. The session may still be running server-side; `/join` re-subscribes.
                    let note = match err {
                        Some(e) => format!("[stream closed: {e}] — /join to resubscribe"),
                        None => "[stream closed] — /join to resubscribe".into(),
                    };
                    j.messages.push(Message::System(note));
                    self.mark_dirty();
                }
                vec![Effect::None]
            }
            Action::GoalMessageOutcome(outcome) => self.on_goal_message_outcome(outcome),
            Action::ConnectionStatus(connected) => {
                let was = self.daemon_connected;
                self.daemon_connected = connected;
                if !connected {
                    self.pending_load = None;
                }
                if was && !connected {
                    self.mark_dirty();
                    self.system_msg("Connection to daemon lost — reconnecting…", Effect::None)
                } else if !was && connected {
                    self.mark_dirty();
                    self.system_msg("Reconnected to daemon.", Effect::None)
                } else {
                    vec![Effect::None]
                }
            }
            // Heartbeat only; animation frames are driven by `needs_animation` in the draw loop.
            Action::Tick => vec![Effect::None],
        }
    }

    fn on_model_selected(
        &mut self,
        model: String,
        error: Option<String>,
        conversation_scoped: bool,
    ) -> Vec<Effect> {
        if let Some(err) = error {
            self.messages
                .push(Message::System(format!("Failed to switch model: {err}")));
        } else {
            if conversation_scoped {
                self.messages.push(Message::System(format!(
                    "Model `{model}` set for this conversation — its next turn uses it \
                 (other chats unchanged)."
                )));
            } else {
                if let Some(st) = self.status.as_mut() {
                    st.model_name = Some(model.clone());
                }
                self.messages.push(Message::System(format!(
                    "Active model switched to `{model}` — next chat turns use it \
                 (no daemon restart)."
                )));
            }
            self.close_model_browser();
        }
        self.scroll_offset = 0;
        self.mark_dirty();
        vec![Effect::None]
    }

    fn on_history_loaded(
        &mut self,
        id: String,
        messages: Vec<ChatMessage>,
        turn_running: bool,
        turn_unanswered: bool,
    ) -> Vec<Effect> {
        if self.pending_load.as_deref() != Some(&id) {
            return vec![Effect::None]; // stale — newer request superseded this one
        }
        self.session = Some(id.clone());
        self.pending_load = None;
        self.messages.clear();
        self.chat_cursor = 0;
        self.turn_offset = 0;
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
            // Count the human's turns being dropped *before* dropping them, so the turn
            // numbers rendered beside what survives still agree with the server's — which
            // counts from the real first turn. Otherwise `/fork 3` would branch somewhere
            // other than the "3" you can see on screen.
            self.turn_offset = self.messages[..removed]
                .iter()
                .filter(|m| matches!(m, Message::User(_)))
                .count();
            self.messages = self.messages.split_off(removed);
            self.messages.insert(
                0,
                Message::System(format!("... {removed} earlier messages omitted")),
            );
        }
        // Turn lifecycle: a missing reply is either still coming or permanently lost.
        // Silence is the wrong reading of either — attach, or say the turn died.
        let mut follow_up = Vec::new();
        if turn_running {
            self.streaming = true;
            self.messages.push(Message::System(
                "A turn is still running — reattaching…".into(),
            ));
            follow_up.push(Effect::AttachConversationStream(id));
        } else if turn_unanswered {
            self.streaming = false;
            self.messages.push(Message::System(
                "The last turn ended without a reply (usually the daemon restarted \
             mid-inference). Nothing is still running — re-send your question to \
             try again."
                    .into(),
            ));
        }
        self.scroll_offset = 0;
        self.focus = Focus::Input;
        self.mark_dirty();
        if follow_up.is_empty() {
            vec![Effect::None]
        } else {
            follow_up
        }
    }

    fn on_goal_message_outcome(&mut self, outcome: GoalMessageOutcome) -> Vec<Effect> {
        use crate::api::GoalMessageOutcome as O;
        match outcome {
            O::Accepted => {} // the echo arrives via the stream as a `human_input` event
            O::NotFound | O::NotPermitted | O::Parked | O::Finished | O::Error(_) => {
                let msg = match outcome {
                    O::NotFound => "[this session is gone — /back to return to chat]".into(),
                    O::NotPermitted => {
                        // Authority, not timing. Waiting will not help; the grant is the fix.
                        "[this session was never allowed to be answered — its profile \
                     grants no AskHuman]"
                            .into()
                    }
                    O::Parked => {
                        // It has NOT finished, and saying it had is the difference between
                        // "start over" and "wait".
                        "[this session is parked — it was waiting on you when the daemon \
                     restarted, and cannot take an answer until it is resumed]"
                            .into()
                    }
                    O::Finished => "[this session has finished — /back to return to chat]".into(),
                    O::Error(e) => format!("[could not deliver message: {e}]"),
                    O::Accepted => unreachable!(),
                };
                if let Some(j) = self.joined.as_mut() {
                    j.messages.push(Message::System(msg));
                } else {
                    self.messages.push(Message::System(msg));
                }
                self.mark_dirty();
            }
        }
        vec![Effect::None]
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

/// Map a pack `domain` string (`"life"` / `"coding"` / anything else) to its display [`SessionKind`].
/// Shared by the offer and spawn paths, which only have the domain as a string.
pub(crate) fn kind_from_domain_str(domain: &str) -> SessionKind {
    let wire = match domain {
        "life" => chat_client_contract::DomainWire::Life,
        "coding" => chat_client_contract::DomainWire::Coding,
        other => chat_client_contract::DomainWire::Custom(other.to_string()),
    };
    SessionKind::from_domain(&wire)
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
