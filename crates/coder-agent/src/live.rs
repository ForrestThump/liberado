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

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_session::SessionEvent;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn scoped_sink_streams_every_emitted_event() {
        let (tx, mut rx) = mpsc::channel::<SessionEvent>(16);
        with_live_events(tx, "s1", async {
            emit(SessionEventKind::Progress {
                message: "hello".into(),
            });
            emit(SessionEventKind::Progress {
                message: "world".into(),
            });
        })
        .await;

        let mut got = Vec::new();
        while let Ok(e) = rx.try_recv() {
            match e.kind {
                SessionEventKind::Progress { message } => got.push(message),
                _ => panic!("unexpected event kind"),
            }
        }
        assert_eq!(got, vec!["hello".to_string(), "world".to_string()]);
    }

    #[tokio::test]
    async fn events_carry_the_scoped_session_id() {
        let (tx, mut rx) = mpsc::channel::<SessionEvent>(4);
        with_live_events(tx, "goal-42", async {
            emit(SessionEventKind::Token { text: "x".into() });
        })
        .await;

        let event = rx.recv().await.unwrap();
        assert_eq!(event.session_id, "goal-42");
    }

    #[tokio::test]
    async fn unscoped_emit_is_a_silent_noop() {
        // No `with_live_events` scope is installed: the task-local lookup fails and emit must
        // neither panic nor deliver anything.
        emit(SessionEventKind::Progress {
            message: "dropped".into(),
        });
    }

    #[tokio::test]
    async fn full_consumer_drops_the_frame_without_blocking_the_run() {
        let (tx, _rx) = mpsc::channel::<SessionEvent>(1);
        with_live_events(tx, "s", async {
            emit(SessionEventKind::Progress {
                message: "will be dropped".into(),
            });
            // The run keeps going even though the sink has no room.
            emit(SessionEventKind::Token {
                text: "still alive".into(),
            });
        })
        .await;
    }
}
