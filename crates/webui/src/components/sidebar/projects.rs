//! Plugin rows in the sidebar, and the project stored on a chat.
//!
//! The Projects heading in the sidebar opens the projects page. Recent project
//! chats stay in the normal chat list.

use dioxus::prelude::*;

use super::rail::PluginGlyph;
use crate::components::project_data::close_phone_drawer;

pub(super) fn project_for_open(
    loaded: Option<&Result<super::ConversationPage, String>>,
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

#[component]
pub(super) fn PluginList(on_open: EventHandler<()>, collapsed: Signal<bool>) -> Element {
    rsx! {
        ul {
            class: "grok-list",
            li {
                class: "grok-row",
                button {
                    class: "grok-row-btn",
                    r#type: "button",
                    onclick: move |_| {
                        on_open.call(());
                        close_phone_drawer(collapsed);
                    },
                    PluginGlyph {}
                    span { class: "grok-row-title", "Manage plugins" }
                }
            }
        }
    }
}
