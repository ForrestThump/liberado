//! The project stored on a chat.
//!
//! Project chats stay in the normal chat list. The Projects button in the
//! primary nav opens the projects page.

use super::ConversationPage;

pub(super) fn project_for_open(
    loaded: Option<&Result<ConversationPage, String>>,
    id: &str,
) -> Option<String> {
    loaded?
        .as_ref()
        .ok()?
        .headers
        .iter()
        .find(|header| header.id == id)?
        .project
        .clone()
}
