//! Wire role for one stored transcript node.
//!
//! The provider role is the default. A failed turn is stored as an assistant message so the
//! next model call still sees it, and stamped with [`liberado_main_agent::TURN_ERROR_AUTHOR`]
//! so the WebUI can render an error bubble.

use liberado_conversation_store::Author;
use liberado_main_agent::TURN_ERROR_AUTHOR;
use liberado_provider::Role;

/// JSON `error` for `POST /api/chat`.
///
/// A missing conversation is a store `NotFound` before any model call. The
/// client needs that text. A turn that ran and failed uses the same sentence as
/// the transcript note. A model rejection names the profile and the model. The
/// provider body stays out of the response.
pub(super) async fn client_turn_error(
    sessions: &liberado_main_agent::ChatSessions,
    session: liberado_conversation_store::Ulid,
    err: &liberado_main_agent::SessionError,
) -> String {
    match err {
        liberado_main_agent::SessionError::Store(
            liberado_conversation_store::StoreError::NotFound(_),
        ) => liberado_common::redact_secrets(&err.to_string()),
        _ => liberado_main_agent::public_failure_text(sessions, session, err).await,
    }
}

pub(super) fn role_for(node: &liberado_conversation_store::MessageNode) -> &'static str {
    if let Author::Named(name) = &node.author
        && name == TURN_ERROR_AUTHOR
    {
        return "error";
    }
    match node.message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}
