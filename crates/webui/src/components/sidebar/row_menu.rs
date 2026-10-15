//! Tappable rename and delete menu for one sidebar row.
//!
//! The control is a button, not a hover affordance, so a phone can open it.
//! Delete is a second step. The daemon calls are `PATCH` and `DELETE` on
//! `/api/conversations/{id}` — the same pair the pre-revamp sidebar used.

use dioxus::prelude::*;

/// Conversation titles are display labels. The same limit is on the input and the submit path.
const MAX_TITLE_CHARS: usize = 120;

/// Id shared by the row element and the desktop menu positioner.
pub(super) fn conversation_row_dom_id(id: &str) -> String {
    format!("conversation-row-{id}")
}

/// Trimmed title the rename request may send.
pub(super) fn normalized_title(raw: &str) -> Result<String, &'static str> {
    let title = raw.trim();
    if title.is_empty() {
        return Err("Enter a title.");
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err("Keep the title under 120 characters.");
    }
    Ok(title.to_string())
}

/// `Some` when this row must not be deleted. The button stays visible and says why.
pub(super) fn blocked_delete(reason: Option<&'static str>) -> Option<&'static str> {
    reason.filter(|text| !text.is_empty())
}

/// Shown on the built-in Agent Creator. Ordinary agent sessions can be deleted.
pub(super) const BUILTIN_AGENT_DELETE_REASON: &str =
    "The built-in Agent Creator cannot be deleted.";

/// `GET /api/profiles` is the only profile route. A profile definition cannot
/// be renamed or deleted from the sidebar.
pub(super) const PROFILE_MUTATION_GAP: &str = "Session profiles are set in topology.toml. The daemon has no endpoint to rename or delete one.";

pub(super) fn agent_delete_block(agent_creator: bool) -> Option<&'static str> {
    if agent_creator {
        Some(BUILTIN_AGENT_DELETE_REASON)
    } else {
        None
    }
}

pub(super) fn delete_block_for(id: &str, creator_ids: &[String]) -> Option<&'static str> {
    agent_delete_block(creator_ids.iter().any(|row| row == id))
}

/// Keep the desktop popover on screen. The sheet is shorter than this reserve.
const DESKTOP_MENU_MAX_HEIGHT: f64 = 240.0;
const DESKTOP_MENU_GUTTER: f64 = 8.0;

fn clamped_desktop_menu_top(row_top: f64, viewport_height: f64) -> f64 {
    let max_top =
        (viewport_height - DESKTOP_MENU_MAX_HEIGHT - DESKTOP_MENU_GUTTER).max(DESKTOP_MENU_GUTTER);
    row_top.clamp(DESKTOP_MENU_GUTTER, max_top)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActionView {
    Actions,
    Rename,
    ConfirmDelete,
}

#[cfg(target_arch = "wasm32")]
fn desktop_menu_style(row_dom_id: &str) -> String {
    let Some(window) = web_sys::window() else {
        return String::new();
    };
    let Some(row) = window
        .document()
        .and_then(|document| document.get_element_by_id(row_dom_id))
    else {
        return String::new();
    };
    let bounds = row.get_bounding_client_rect();
    let viewport_height = window
        .inner_height()
        .ok()
        .and_then(|height| height.as_f64())
        .unwrap_or(720.0);
    let top = clamped_desktop_menu_top(bounds.top(), viewport_height);
    let left = bounds.right() + DESKTOP_MENU_GUTTER;
    format!("--conv-menu-top:{top}px;--conv-menu-left:{left}px")
}

#[cfg(not(target_arch = "wasm32"))]
fn desktop_menu_style(_row_dom_id: &str) -> String {
    String::new()
}

#[component]
pub(super) fn RowOptions(
    id: String,
    /// Full title for the rename field. Empty when the row has none.
    seed: String,
    /// Short label shown on the menu. Already truncated for the sidebar.
    display: String,
    /// "chat" or "agent" — the sentence the menu uses for this row.
    noun: &'static str,
    menu_for: Signal<Option<String>>,
    action_error: Signal<Option<String>>,
    /// When set, Delete is visible but does not run. The text is the reason.
    delete_block: Option<&'static str>,
    on_delete: EventHandler<String>,
    on_rename: EventHandler<(String, String)>,
) -> Element {
    let mut action_view = use_signal(|| ActionView::Actions);
    let mut draft_title = use_signal(|| seed.clone());
    let mut validation_error = use_signal(|| None::<&'static str>);
    let mut menu_position_style = use_signal(String::new);
    let menu_open = menu_for().as_deref() == Some(id.as_str());
    let row_dom_id = conversation_row_dom_id(&id);
    let block = blocked_delete(delete_block);

    let open_menu = {
        let id = id.clone();
        let seed = seed.clone();
        let row_dom_id = row_dom_id.clone();
        move |evt: Event<MouseData>| {
            evt.prevent_default();
            evt.stop_propagation();
            action_view.set(ActionView::Actions);
            draft_title.set(seed.clone());
            validation_error.set(None);
            menu_position_style.set(desktop_menu_style(&row_dom_id));
            action_error.set(None);
            menu_for.set(Some(id.clone()));
        }
    };
    let close_menu = move |_: Event<MouseData>| {
        validation_error.set(None);
        menu_for.set(None);
        action_error.set(None);
    };

    rsx! {
        button {
            class: "grok-row-options",
            r#type: "button",
            title: "Rename or delete",
            "aria-label": "Options for {display}",
            "aria-haspopup": "menu",
            "aria-expanded": "{menu_open}",
            onclick: open_menu,
            "…"
        }
        if menu_open {
            div {
                class: "conv-menu-backdrop",
                style: "{menu_position_style}",
                onclick: close_menu,
                oncontextmenu: move |evt| {
                    evt.prevent_default();
                    evt.stop_propagation();
                    validation_error.set(None);
                    menu_for.set(None);
                    action_error.set(None);
                },
                div {
                    class: "conv-menu",
                    role: "menu",
                    onclick: move |evt: Event<MouseData>| evt.stop_propagation(),
                    if let Some(message) = action_error() {
                        p { class: "conv-menu-error server", "{message}" }
                    }
                    match action_view() {
                        ActionView::Actions => rsx! {
                            p { class: "conv-menu-label", "{display}" }
                            p { class: "conv-menu-note", "Choose what to do with this {noun}." }
                            div {
                                class: "conv-menu-actions stacked",
                                button {
                                    class: "conv-menu-btn",
                                    r#type: "button",
                                    onclick: move |_| {
                                        validation_error.set(None);
                                        action_view.set(ActionView::Rename);
                                    },
                                    "Rename"
                                }
                                if let Some(reason) = block {
                                    button {
                                        class: "conv-menu-btn danger",
                                        r#type: "button",
                                        disabled: true,
                                        title: "{reason}",
                                        "Delete"
                                    }
                                    p { class: "conv-menu-note", "{reason}" }
                                } else {
                                    button {
                                        class: "conv-menu-btn danger",
                                        r#type: "button",
                                        onclick: move |_| {
                                            validation_error.set(None);
                                            action_view.set(ActionView::ConfirmDelete);
                                        },
                                        "Delete"
                                    }
                                }
                                button {
                                    class: "conv-menu-btn",
                                    r#type: "button",
                                    onclick: move |_| {
                                        menu_for.set(None);
                                        action_error.set(None);
                                    },
                                    "Cancel"
                                }
                            }
                        },
                        ActionView::Rename => rsx! {
                            form {
                                onsubmit: {
                                    let id = id.clone();
                                    move |evt| {
                                        evt.prevent_default();
                                        match normalized_title(draft_title.read().as_str()) {
                                            Ok(new_title) => {
                                                validation_error.set(None);
                                                on_rename.call((id.clone(), new_title));
                                            }
                                            Err(message) => validation_error.set(Some(message)),
                                        }
                                    }
                                },
                                p { class: "conv-menu-label", "Rename {noun}" }
                                input {
                                    class: "conv-menu-input",
                                    r#type: "text",
                                    maxlength: "{MAX_TITLE_CHARS}",
                                    value: "{draft_title}",
                                    autofocus: true,
                                    "aria-label": "New name",
                                    oninput: move |evt| {
                                        draft_title.set(evt.value());
                                        validation_error.set(None);
                                    },
                                }
                                if let Some(message) = validation_error() {
                                    p { class: "conv-menu-error", "{message}" }
                                }
                                div {
                                    class: "conv-menu-actions",
                                    button {
                                        class: "conv-menu-btn primary",
                                        r#type: "submit",
                                        "Save"
                                    }
                                    button {
                                        class: "conv-menu-btn",
                                        r#type: "button",
                                        onclick: move |_| {
                                            validation_error.set(None);
                                            action_view.set(ActionView::Actions);
                                        },
                                        "Back"
                                    }
                                }
                            }
                        },
                        ActionView::ConfirmDelete => rsx! {
                            p { class: "conv-menu-label", "Delete permanently?" }
                            p { class: "conv-menu-note", "Removes it from disk. There is no undo." }
                            div {
                                class: "conv-menu-actions",
                                button {
                                    class: "conv-menu-btn danger",
                                    r#type: "button",
                                    onclick: {
                                        let id = id.clone();
                                        move |_| on_delete.call(id.clone())
                                    },
                                    "Delete"
                                }
                                button {
                                    class: "conv-menu-btn",
                                    r#type: "button",
                                    onclick: move |_| action_view.set(ActionView::Actions),
                                    "Back"
                                }
                            }
                        },
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DESKTOP_MENU_GUTTER, blocked_delete, clamped_desktop_menu_top, conversation_row_dom_id,
        normalized_title,
    };

    #[test]
    fn rename_titles_are_trimmed_and_bounded() {
        assert_eq!(
            normalized_title("  A better name  "),
            Ok("A better name".into())
        );
        assert_eq!(normalized_title("   "), Err("Enter a title."));
        let too_long = "x".repeat(121);
        assert_eq!(
            normalized_title(&too_long),
            Err("Keep the title under 120 characters.")
        );
    }

    #[test]
    fn a_delete_block_keeps_the_reason() {
        assert_eq!(blocked_delete(None), None);
        assert_eq!(blocked_delete(Some("")), None);
        assert_eq!(
            blocked_delete(Some("The built-in Agent Creator cannot be deleted.")),
            Some("The built-in Agent Creator cannot be deleted.")
        );
    }

    #[test]
    fn the_options_control_is_a_button_and_delete_is_a_second_step() {
        let src = include_str!("row_menu.rs");
        let button = src
            .split("class: \"grok-row-options\"")
            .nth(1)
            .expect("options button");
        let button = button.split("if menu_open").next().unwrap_or(button);
        assert!(button.contains("r#type: \"button\""));
        assert!(button.contains("aria-label"));
        assert!(
            !button.contains("on_delete"),
            "the options button must not delete"
        );
        let confirm = src
            .split("ActionView::ConfirmDelete =>")
            .nth(1)
            .expect("confirm view");
        assert!(confirm.contains("Delete permanently?"));
        assert!(confirm.contains("on_delete.call"));
        let actions = src.split("ActionView::Actions =>").nth(1).expect("actions");
        let actions = actions
            .split("ActionView::Rename =>")
            .next()
            .unwrap_or(actions);
        assert!(
            !actions.contains("on_delete.call"),
            "the first Delete only opens confirmation"
        );
        assert!(actions.contains("disabled: true"));
    }

    #[test]
    fn row_ids_match_the_element_the_menu_measures() {
        assert_eq!(conversation_row_dom_id("abc"), "conversation-row-abc");
    }

    #[test]
    fn desktop_action_sheet_stays_inside_the_viewport() {
        assert_eq!(clamped_desktop_menu_top(100.0, 900.0), 100.0);
        assert_eq!(clamped_desktop_menu_top(-20.0, 900.0), DESKTOP_MENU_GUTTER);
        assert_eq!(clamped_desktop_menu_top(850.0, 900.0), 652.0);
    }
}
