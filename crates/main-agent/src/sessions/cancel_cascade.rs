//! Cancelling a chat turn, and the hub sessions that turn started.
//!
//! The cascade is the reason this is its own module: a cancel reaches two systems that know
//! nothing about each other — the turn registry here, and the goal hub, which is addressed by
//! conversation id and answers asynchronously.

use liberado_conversation_store::Ulid;
use liberado_executor::AgentEvent;

use super::ChatSessions;

impl ChatSessions {
    /// Stop the turn running for `session`, and the hub sessions that turn started. `true` if
    /// there was a turn.
    ///
    /// The explicit counterpart to what a disconnect used to do implicitly. It has to exist *before*
    /// turns are detached, or there would be no way to stop one at all — closing the stream was the
    /// only cancel this system had.
    ///
    /// The cascade is the other half. A `delegate` call blocks the chat turn on a hub session;
    /// aborting the turn dropped the `await_terminal` future and nothing more, so the child ran on
    /// with its own grant and nobody waiting for it. Cancel now reaches the whole tree the
    /// conversation started.
    pub fn cancel_turn(&self, session: Ulid) -> bool {
        // Cascade first: the children are addressed by conversation id, not by the turn entry, so
        // this is the same work whether or not a turn is in flight. A cancel that arrives just
        // after the turn retired still has to stop what the turn left running.
        self.cancel_sessions_from(session);
        let entry = {
            let mut running = self.running.lock().unwrap_or_else(|p| p.into_inner());
            running.remove(&session)
        };
        let Some(entry) = entry else {
            return false;
        };
        if let Some(handle) = entry.abort.lock().unwrap_or_else(|p| p.into_inner()).take() {
            handle.abort();
        }
        // Tell anyone watching why their stream ended. Without this a cancelled turn looks
        // indistinguishable from one still thinking.
        entry.publish(AgentEvent::Error("turn cancelled".into()));
        true
    }

    /// Cancel every live hub session whose origin is `conversation`.
    ///
    /// Detached, because `cancel_turn` is synchronous and the hub is not: a caller that aborts a
    /// turn must not block on a pack noticing its cancel token. Off a Tokio runtime there is
    /// nowhere to detach to, which only happens in a non-async caller, so say so rather than panic.
    fn cancel_sessions_from(&self, conversation: Ulid) {
        let Some(hub) = self.goals.clone() else {
            return; // no hub attached: nothing was ever delegated
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                conversation = %conversation,
                "cancel_turn called off a Tokio runtime; delegated sessions were not cancelled"
            );
            return;
        };
        let origin = conversation.to_string();
        handle.spawn(async move {
            for record in hub.list().await {
                if record.status.is_terminal() {
                    continue;
                }
                let started_here = record
                    .goal
                    .origin
                    .as_ref()
                    .and_then(|o| o.conversation_id.as_deref())
                    == Some(origin.as_str());
                if !started_here {
                    continue;
                }
                // A session that finished between `list` and here reports "not found or already
                // finished". That is the outcome we wanted, so it is not worth a warning.
                if let Err(error) = hub.cancel(&record.id).await {
                    tracing::debug!(
                        session = %record.id,
                        %error,
                        "delegated session was already gone when the chat turn was cancelled"
                    );
                }
            }
        });
    }
}
