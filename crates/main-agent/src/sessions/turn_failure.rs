//! Record a short, secret-free note when a chat turn fails.
//!
//! The assistant tail still persists only on success. This note is a separate node so a
//! client that reloads history can show the failure. Cancellation drops the turn future
//! before [`keep_or_record`] runs, so a cancel does not write the note.
//!
//! A model rejection names the session profile and the model that was sent. Those names
//! come from the session, not from the provider body, so a secret in the raw error stays
//! out of the sentence.

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

/// Sentence used when a rejection has no profile and no model to name.
const MODEL_REJECTED: &str = "The model rejected this request. Check the model name and try again.";

/// The sentence a person sees. It is a class of failure, never the raw error.
///
/// A model rejection from this function does not name the profile or the model.
/// Call [`public_failure_text`] when the session is known.
pub fn public_turn_failure(err: &SessionError) -> &'static str {
    match err {
        SessionError::Exec(ExecError::Provider(ProviderError::RateLimited)) => {
            "The model is rate limited. Wait a moment and send again."
        }
        SessionError::Exec(ExecError::Provider(ProviderError::InvalidRequest(_))) => MODEL_REJECTED,
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

/// The sentence a person sees for this session.
///
/// Model rejections name the profile and the model that was sent. Every other class
/// stays on [`public_turn_failure`].
pub async fn public_failure_text(
    sessions: &ChatSessions,
    session: Ulid,
    err: &SessionError,
) -> String {
    if !is_model_rejection(err) {
        return public_turn_failure(err).to_string();
    }
    let (profile, model) = reject_names(sessions, session).await;
    named_rejection(profile.as_deref(), model.as_deref())
}

/// Log the redacted cause and return the short event a client renders.
pub async fn failure_event(
    sessions: &ChatSessions,
    session: Ulid,
    err: &SessionError,
) -> AgentEvent {
    let error = liberado_common::redact_secrets(&err.to_string());
    tracing::warn!(%error, %session, "chat turn failed");
    AgentEvent::Error(public_failure_text(sessions, session, err).await)
}

fn is_model_rejection(err: &SessionError) -> bool {
    matches!(
        err,
        SessionError::Exec(ExecError::Provider(ProviderError::InvalidRequest(_)))
    )
}

fn named_rejection(profile: Option<&str>, model: Option<&str>) -> String {
    match (profile, model) {
        (Some(profile), Some(model)) => format!(
            "The model rejected this request. Profile '{profile}', model '{model}'. \
             Check the model name and try again."
        ),
        (None, Some(model)) => format!(
            "The model rejected this request. Model '{model}'. \
             Check the model name and try again."
        ),
        (Some(profile), None) => format!(
            "The model rejected this request. Profile '{profile}'. \
             Check the model name and try again."
        ),
        (None, None) => MODEL_REJECTED.to_string(),
    }
}

async fn reject_names(sessions: &ChatSessions, session: Ulid) -> (Option<String>, Option<String>) {
    let header = sessions.store.header(session).await.ok();
    let profile = header
        .as_ref()
        .and_then(|header| header.grant.profile.clone())
        .filter(|name| !name.is_empty());
    let stamped = sessions
        .model_last_used(session)
        .await
        .filter(|name| !name.is_empty());
    let from_profile = header
        .and_then(|header| header.grant.model)
        .filter(|name| !name.is_empty());
    (profile, stamped.or(from_profile))
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
        let text = public_failure_text(sessions, session, err).await;
        record(sessions, session, &text).await;
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
