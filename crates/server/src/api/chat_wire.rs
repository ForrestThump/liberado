//! Wire role for one stored transcript node.
//!
//! The provider role is the default. A failed turn is stored as an assistant message so the
//! next model call still sees it, and stamped with [`liberado_main_agent::TURN_ERROR_AUTHOR`]
//! so the WebUI can render an error bubble.

use liberado_conversation_store::Author;
use liberado_main_agent::TURN_ERROR_AUTHOR;
use liberado_provider::Role;

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
