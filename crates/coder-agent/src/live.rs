//! The live event tap: what a surface sees *while* a coding run is happening.
//!
//! A run already records everything it does to a durable trace, but a trace is written at the
//! end. Anything watching in real time — the goal pane, the WebUI, an ACP editor — needs the
//! same events as they happen, and that is what this is.
//!
//! ## Why a task-local rather than a field
//!
//! The emitters are deep inside the tool runtime and the completion gate, several layers below
//! anything that knows a session exists. Threading a sender through every one of them would put
//! a UI concern into signatures that have nothing to do with UIs. A task-local scoped once
//! around the run reaches all of them and disappears when the run ends.
//!
//! The cost is that it is **silently inert when unscoped**: `try_with` fails and every emit
//! becomes a no-op. That is exactly how the ACP bridge shipped with no live output at all — it
//! calls [`LiberadoLoopBackend::run`](crate::LiberadoLoopBackend) directly rather than going
//! through `CodingSessionPack`, which was the only caller that scoped this, so every event in a
//! Paseo run was discarded at birth while the trace recorded them faithfully. [`with_live_events`]
//! exists so any caller can opt in without reaching into pack internals.

use liberado_session::{SessionEvent, SessionEventKind};
use tokio::sync::mpsc;

tokio::task_local! {
    /// Scoped for the duration of a run: `(sink, session_id)`. See [`with_live_events`].
    pub(crate) static LIVE_GATE: (mpsc::Sender<SessionEvent>, String);
}

/// Run `fut` with a live event sink installed, so everything it emits streams to `tx`.
///
/// Scope this around the whole coding run — not a phase of it — or events from the phases you
/// missed vanish with no indication that they were dropped.
pub async fn with_live_events<F, T>(
    tx: mpsc::Sender<SessionEvent>,
    session_id: impl Into<String>,
    fut: F,
) -> T
where
    F: std::future::Future<Output = T>,
{
    LIVE_GATE.scope((tx, session_id.into()), fut).await
}

/// Best-effort mirror onto the live bus. A no-op when no sink is scoped.
///
/// `try_send`, never `send`: a slow or gone consumer must not stall the run. Dropping a frame
/// degrades the view; blocking the tool loop on a UI would be a far worse trade, and the trace
/// remains the complete record either way.
pub(crate) fn emit(kind: SessionEventKind) {
    let Ok((tx, session_id)) = LIVE_GATE.try_with(|(tx, id)| (tx.clone(), id.clone())) else {
        return;
    };
    let _ = tx.try_send(SessionEvent::new(session_id, kind));
}
