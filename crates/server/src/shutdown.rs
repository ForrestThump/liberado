//! Graceful shutdown drain for durable **chat turns and goal sessions**.
//!
//! Durable work outlives its HTTP connection. Docker/SIGTERM killing the process mid-run loses
//! work that could still finish and persist. This module:
//!
//! 1. Marks the daemon as **not accepting new work** ([`DrainGate`]) — clients get a
//!    distinguishable `shutting_down` refusal, not a generic failure.
//! 2. Waits up to a **bounded grace period** for in-flight chat turns **and** goal sessions.
//! 3. On grace timeout: **aborts** remaining chat turns; **parks** remaining goal sessions (so
//!    post-drain status is not permanently `Running` — parked is human-actionable; aborted chat
//!    turns read as unanswered). Prefer park over goal-cancel: a parked session can be resumed
//!    when the pack allows; a cancelled one cannot.
//!
//! Resume of mid-inference model calls across restart is out of scope (inference is not
//! replayable). Parking is session-level only.
//!
//! # Work-start inventory (what is gated during drain)
//!
//! | Surface | Starts work how | Gated? |
//! |---|---|---|
//! | Chat HTTP `POST /api/chat`, `/api/chat/stream` | route layer [`refuse_new_turns_if_draining`] | **yes** |
//! | Chat Telegram free-form | `TelegramChatBridge` checks `drain.is_accepting()` | **yes** (capability, not this middleware) |
//! | Goals HTTP `POST /api/goals` | same refuse middleware as chat starts | **yes** |
//! | Goals HTTP cancel/park/message/stream/list | do not start new goal work | no (manage in-flight) |
//! | Hooks `POST /api/hooks/{name}` | `trigger_hook` checks `drain.is_accepting()` | **yes** |
//! | Cron | scheduled fires into the daemon event loop | **not** HTTP — loop stops after drain; no new cron processing once process exits |
//! | Vault reactions | watcher → reactions while daemon runs | **not** HTTP — stopped with the process after drain |
//!
//! The gate covers **capabilities that start work**, not every route someone remembered. Chat
//! attach/cancel stay open so clients can rejoin or stop work already in flight.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use axum::middleware::Next;
use axum::response::IntoResponse;
use tracing::{info, warn};

use crate::state::AppState;

/// Default in-process grace when `LIBERADO_SHUTDOWN_GRACE_SECS` is unset.
///
/// Long research turns routinely exceed 200s; median delegating turn is ~26k tokens over ~4 hops
/// (each hop a model roundtrip).  Docker's default stop timeout is 10s — production compose must
/// set `stop_grace_period` ≥ this value so the container is not SIGKILL'd during drain.  Operators
/// who need less (short chat, no delegation) can set `LIBERADO_SHUTDOWN_GRACE_SECS=90` or lower.
pub const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(300);

/// Wire error kind clients can match on when new turns are refused during drain.
pub const SHUTTING_DOWN_ERROR: &str = "shutting_down";

/// How long the drain lets a cooperative park land before recording the status itself.
///
/// Deliberately short: this is a courtesy window for a pack that is between turns, not a second
/// grace period. A pack that needs longer is one blocked in a model call, and no achievable wait
/// helps it — `force_park_still_hosted` is what makes that case correct.
const PARK_SETTLE_MS: u64 = 500;

/// Human-readable refusal for surfaces with no JSON envelope to put [`SHUTTING_DOWN_ERROR`] in —
/// today the Telegram bridge, which calls `ChatSessions` directly and so never passes through
/// [`refuse_new_turns_if_draining`]. Kept beside the wire constant so the two stay one decision.
pub const SHUTTING_DOWN_MESSAGE: &str =
    "The daemon is restarting and is not accepting new turns. Try again in a moment.";

/// Process-wide gate: when false, turn-starting HTTP routes refuse with [`shutting_down_response`].
#[derive(Debug)]
pub struct DrainGate {
    /// `true` while the daemon accepts new chat turns (normal operation).
    accepting: AtomicBool,
}

impl Default for DrainGate {
    fn default() -> Self {
        Self {
            accepting: AtomicBool::new(true),
        }
    }
}

impl DrainGate {
    pub fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::SeqCst)
    }

    /// Enter drain: new turns are refused. Idempotent.
    pub fn begin_drain(&self) {
        self.accepting.store(false, Ordering::SeqCst);
    }
}

/// Outcome of [`drain_for_shutdown`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainOutcome {
    /// True if every in-flight chat turn **and** goal session finished before grace elapsed.
    pub idle_within_grace: bool,
    /// How long we waited (≤ grace).
    pub waited: Duration,
    /// Chat turns still running when grace ended (aborted via `cancel_turn`).
    pub aborted: usize,
    /// Goal sessions still hosted when grace ended (park signals sent).
    pub parked_goals: usize,
    /// Grace budget that was applied.
    pub grace: Duration,
}

/// Resolve grace from env `LIBERADO_SHUTDOWN_GRACE_SECS` or [`DEFAULT_SHUTDOWN_GRACE`].
pub fn shutdown_grace_from_env() -> Duration {
    std::env::var("LIBERADO_SHUTDOWN_GRACE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_SHUTDOWN_GRACE)
}

/// Distinctive JSON body for refused new turns during drain.
pub fn shutting_down_json() -> serde_json::Value {
    serde_json::json!({
        "error": SHUTTING_DOWN_ERROR,
        "message": "daemon is shutting down; new turns are not accepted",
    })
}

pub fn shutting_down_response() -> Response<Body> {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [("content-type", "application/json")],
        shutting_down_json().to_string(),
    )
        .into_response()
}

/// Axum middleware for **work-starting** routes only (`/api/chat`, `/api/chat/stream`,
/// `POST /api/goals`). Attach/cancel/park/list stay available so clients can rejoin or stop work
/// already in flight.
pub async fn refuse_new_turns_if_draining(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response<Body> {
    if !state.drain.is_accepting() {
        return shutting_down_response();
    }
    next.run(request).await
}

/// Mark drain, wait up to `grace` for in-flight chat turns **and** goal sessions, then abort chat
/// stragglers and **park** remaining goals.
///
/// Pure coordination over [`AppState`] — no SIGTERM plumbing; `run` calls this from the signal
/// handler. Tests call it directly with a short grace.
pub async fn drain_for_shutdown(state: &AppState, grace: Duration) -> DrainOutcome {
    state.drain.begin_drain();
    info!(
        grace_secs = grace.as_secs(),
        "shutdown drain: refusing new work; waiting for in-flight chat + goal sessions"
    );

    let start = Instant::now();
    let idle = wait_until_idle(state, grace).await;
    let waited = start.elapsed();

    if idle {
        info!(
            waited_ms = waited.as_millis() as u64,
            "shutdown drain: all in-flight work finished within grace"
        );
        return DrainOutcome {
            idle_within_grace: true,
            waited,
            aborted: 0,
            parked_goals: 0,
            grace,
        };
    }

    let (aborted, parked_goals) = abort_and_park(state, waited).await;
    DrainOutcome {
        idle_within_grace: false,
        waited,
        aborted,
        parked_goals,
        grace,
    }
}

/// Grace elapsed with work still in flight: abort chat stragglers, park remaining goal sessions,
/// and let the park/cancel settle so nothing is left `Running` with no host.
async fn abort_and_park(state: &AppState, waited: Duration) -> (usize, usize) {
    let aborted = abort_remaining_turns(state);
    let parked_goals = park_remaining_goals(state).await;
    // Cooperative park/cancel needs a moment to leave the running map and flip status.
    let _ = wait_until_idle(state, Duration::from_millis(PARK_SETTLE_MS)).await;
    // A pack blocked in a model call cannot observe the signal before we exit, and `Parked` is
    // only filed when a pack returns. Record it ourselves so nothing is left `Running` with no
    // host — see `GoalSessionHub::force_park_still_hosted`.
    let forced_park_goals = state.goals.force_park_still_hosted().await;
    warn!(
        aborted,
        parked_goals,
        forced_park_goals,
        waited_ms = waited.as_millis() as u64,
        "shutdown drain: grace elapsed; aborted chat stragglers and parked remaining goals"
    );
    (aborted, parked_goals)
}

/// Poll until chat + goal in-flight counts are both 0, or `grace` elapses.
///
/// Returns true if idle within grace. A **zero grace** returns immediately (does not wait) —
/// that is intentional so tests can prove non-zero grace is what makes finish-within-grace work.
pub async fn wait_until_idle(state: &AppState, grace: Duration) -> bool {
    if grace.is_zero() {
        return total_in_flight(state).await == 0;
    }
    let deadline = Instant::now() + grace;
    loop {
        if total_in_flight(state).await == 0 {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn total_in_flight(state: &AppState) -> usize {
    chat_in_flight_count(state) + state.goals.in_flight_count().await
}

fn chat_in_flight_count(state: &AppState) -> usize {
    state
        .chat
        .as_ref()
        .map(|c| c.in_flight_count())
        .unwrap_or(0)
}

/// Cancel every still-running chat turn so post-drain state is not `turn_running`.
/// Cancel persists nothing → transcripts that ended on a user message read as unanswered.
fn abort_remaining_turns(state: &AppState) -> usize {
    let Some(chat) = state.chat.as_ref() else {
        return 0;
    };
    let ids = chat.in_flight_sessions();
    let mut n = 0;
    for id in ids {
        if chat.cancel_turn(id) {
            n += 1;
        }
    }
    n
}

/// Park every still-hosted goal session so post-drain status is not permanently `Running`.
async fn park_remaining_goals(state: &AppState) -> usize {
    state.goals.park_all_in_flight().await
}

/// Await OS shutdown signals (Ctrl+C; SIGTERM on Unix). Used by `axum::serve` graceful path.
pub async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("received Ctrl+C");
            }
            _ = sigterm.recv() => {
                info!("received SIGTERM");
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        info!("received Ctrl+C");
    }
}

#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "shutdown_grace_tests.rs"]
mod grace_tests;
