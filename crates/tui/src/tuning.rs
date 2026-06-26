//! Tuning constants for the Liberado TUI.
//!
//! All tunable parameters live here so they can be adjusted without hunting through
//! source files. These are compile-time constants — future work could load them from
//! `tuning.toml` (alongside `topology.toml`/`policy.toml`) for no-recompile tweaking.

use std::time::Duration;

// ── Channel capacity ────────────────────────────────────────────────

/// Capacity of the bounded mpsc action channel.
pub const ACTION_CHANNEL_CAPACITY: usize = 256;

// ── Timing ──────────────────────────────────────────────────────────

/// Event loop poll interval. 16 ms ≈ 60 FPS cap on redraws.
pub const POLL_INTERVAL: Duration = Duration::from_millis(16);

/// Maximum idle time waiting for the next SSE chunk before declaring a timeout.
/// Prevents the stream reader from hanging forever on a silent server.
pub const SSE_STREAM_TIMEOUT: Duration = Duration::from_secs(60);

/// Interval between background HTTP polls (status, reactions, conversations).
pub const BACKEND_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Consecutive poll failures before declaring the daemon disconnected.
pub const MAX_POLL_FAILURES: u32 = 2;

/// Number of recent reactions to fetch per poll.
pub const REACTIONS_FETCH_LIMIT: usize = 20;

// ── Scrolling ───────────────────────────────────────────────────────

/// Lines to scroll per mouse wheel tick in the chat pane.
pub const MOUSE_SCROLL_LINES: usize = 3;

/// Lines to scroll on PgUp / PgDn.
pub const PAGE_SCROLL_LINES: usize = 10;

/// Approximate number of visible message lines used for scroll-to-cursor.
pub const CHAT_VISIBLE_LINES: usize = 20;

// ── Layout ──────────────────────────────────────────────────────────

pub const INPUT_AREA_HEIGHT: u16 = 3;
pub const STATUS_BAR_HEIGHT: u16 = 1;
pub const INPUT_ROW_HEIGHT: u16 = 2;
pub const CHAT_SIDEBAR_SPLIT_CHAT: u16 = 70;
pub const CHAT_SIDEBAR_SPLIT_SIDEBAR: u16 = 30;
pub const SIDEBAR_STATUS_HEIGHT: u16 = 6;
pub const SIDEBAR_REACTIONS_MIN_HEIGHT: u16 = 3;
pub const SIDEBAR_CONVERSATIONS_MIN_HEIGHT: u16 = 3;

// ── Truncation lengths ──────────────────────────────────────────────

/// Max displayed length for tool call arguments in the chat and history.
pub const TOOL_ARGS_TRUNCATE: usize = 120;

/// Max displayed length for tool call arguments/preview rendered inline.
pub const TOOL_DISPLAY_TRUNCATE: usize = 80;

/// Max characters shown for the vault path in the status panel.
pub const VAULT_PATH_TRUNCATE: usize = 25;

/// Max characters shown for the server URL in the status panel.
pub const SERVER_URL_TRUNCATE: usize = 20;

/// Reaction event path truncation width.
pub const REACTION_PATH_TRUNCATE: usize = 40;

/// Short identity prefix length (e.g. first 8 chars of a session id).
pub const SHORT_ID_LEN: usize = 8;

/// Heading level at or below which headings render in bold (h1, h2).
pub const HEADING_BOLD_THRESHOLD: usize = 2;

// ── Display thresholds ──────────────────────────────────────────────

/// Cap for the context-usage percentage display so it never shows "100%".
pub const CTX_PCT_DISPLAY_CAP: f64 = 99.9;

// ── Relative-time buckets (seconds → threshold) ─────────────────────

pub const RELATIVE_SECS_THRESHOLD: u64 = 60;
pub const RELATIVE_MINS_THRESHOLD: u64 = 60;
pub const RELATIVE_HOURS_THRESHOLD: u64 = 24;
pub const RELATIVE_YESTERDAY_THRESHOLD: u64 = 1;
pub const RELATIVE_DAYS_THRESHOLD: u64 = 7;

// ── Uptime ──────────────────────────────────────────────────────────

pub const SECS_IN_HOUR: u64 = 3600;
pub const SECS_IN_MINUTE: u64 = 60;

// ── Message cap ─────────────────────────────────────────────────────

/// Maximum number of messages to keep in memory. Older messages are
/// evicted with a system marker inserted at the top when history is
/// loaded. Normal conversation turns typically stay well under this
/// limit, so the cap only applies on `HistoryLoaded` (the source of
/// unbounded growth) rather than pruning mid-conversation.
pub const MAX_MESSAGE_COUNT: usize = 500;

// ── Text helpers ────────────────────────────────────────────────────

/// Number of characters in an ellipsis (always 3 for "…" or "...").
pub const ELLIPSIS_LEN: usize = 3;

/// Spinner animation frames (braille-compatible ASCII).
pub static SPINNER_FRAMES: &[char] = &['|', '/', '-', '\\'];
