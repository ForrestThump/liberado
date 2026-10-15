//! Record a short, secret-free note when a chat turn fails.
//!
//! The assistant tail still persists only on success. This note is a separate node so a
//! client that reloads history can show the failure. Cancellation drops the turn future
//! before [`keep_or_record`] runs, so a cancel does not write the note.

use liberado_conversation_store::{Author, NewNode, Ulid};
use liberado_executor::{AgentEvent, ExecError};
use liberado_provider::{Message, ProviderError};

use super::{ChatSessions, SessionError, SessionResult};

/// [`Author::Named`] identity of a failed-turn note.
///
/// Named, not `Author::System`: a system node is the face prompt, and history readers drop
/// those. The stored message is still an assistant message so the next model call can see
/// that the previous turn failed. The wire role is overridden to `"error"` by this author.
pub const TURN_ERROR_AUTHOR: &str = "error";

/// The sentence a person sees. It is a class of failure, never the raw error.
pub fn public_turn_failure(err: &SessionError) -> &'static str {
    match err {
        SessionError::Exec(ExecError::Provider(ProviderError::RateLimited)) => {
            "The model is rate limited. Wait a moment and send again."
        }
        SessionError::Exec(ExecError::Provider(ProviderError::InvalidRequest(_))) => {
            "The model rejected this request. Check the model name and try again."
        }
        SessionError::Exec(ExecError::Provider(_)) => {
            "The model provider failed before it could reply."
        }
        SessionError::Exec(ExecError::BudgetExceeded { .. }) => {
            "This turn hit its limit before it could finish."
        }
        SessionError::Exec(ExecError::Decode(_)) => {
            "The model returned a reply that could not be read."
        }
        SessionError::Store(_) => "The conversation could not be saved.",
        SessionError::Exec(ExecError::AwaitingHuman { .. } | ExecError::Internal(_)) => {
            "This turn failed before it could finish."
        }
    }
}

/// Log the redacted cause and return the short event a client renders.
pub fn failure_event(session: Ulid, err: &SessionError) -> AgentEvent {
    let error = liberado_common::redact_secrets(&err.to_string());
    tracing::warn!(%error, %session, "chat turn failed");
    AgentEvent::Error(public_turn_failure(err).to_string())
}

/// On `Err`, append the public failure note, then return the original error.
///
/// A store failure while writing the note is logged and does not replace the turn error.
/// `E` is [`ExecError`] at the turn boundary. [`SessionError`] implements `From` for it.
pub async fn keep_or_record<T, E>(
    sessions: &ChatSessions,
    session: Ulid,
    result: Result<T, E>,
) -> SessionResult<T>
where
    E: Into<SessionError>,
{
    let result = result.map_err(Into::into);
    if let Err(err) = &result {
        record(sessions, session, public_turn_failure(err)).await;
    }
    result
}

async fn record(sessions: &ChatSessions, session: Ulid, text: &str) {
    let parent = match sessions.store.leaf_path(session, None).await {
        Ok(nodes) => nodes.last().map(|node| node.id),
        Err(err) => {
            warn_record(&err.to_string(), session);
            return;
        }
    };
    if let Err(err) = sessions
        .store
        .append(
            session,
            NewNode {
                parent_id: parent,
                author: Author::Named(TURN_ERROR_AUTHOR.into()),
                message: Message::assistant(text),
                model: None,
                reasoning: None,
            },
        )
        .await
    {
        warn_record(&err.to_string(), session);
    }
}

fn warn_record(detail: &str, session: Ulid) {
    let error = liberado_common::redact_secrets(detail);
    tracing::warn!(%error, %session, "chat turn failure note was not saved");
}
