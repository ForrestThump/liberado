use dioxus::prelude::*;

use super::sidebar::relative_time;

/// The row's title, or "Untitled" when it is missing or empty. Whitespace-only titles pass through
/// because the guard is `!is_empty()`, not `!trim().is_empty()`.
fn conv_title(title: &Option<String>) -> &str {
    match title {
        Some(title) if !title.is_empty() => title.as_str(),
        _ => "Untitled",
    }
}

/// A hold shorter than this remains an ordinary tap that opens the conversation. Touch movement
/// cancels the hold, so scrolling the sidebar never opens an action menu by accident.
const LONG_PRESS_MS: f64 = 550.0;

/// Conversation titles are display labels, not documents. Keeping the same modest limit in the
/// input and the submit path avoids a pasted essay turning the sidebar into unusable data.
const MAX_TITLE_CHARS: usize = 120;

/// Conservative height used to keep every desktop action-sheet view above the viewport bottom.
/// The actual sheet is usually shorter; reserving the tallest view prevents a Rename/Delete switch
/// from making it jump.
const DESKTOP_MENU_MAX_HEIGHT: f64 = 240.0;
const DESKTOP_MENU_GUTTER: f64 = 8.0;

fn normalized_title(raw: &str) -> Result<String, &'static str> {
    let title = raw.trim();
    if title.is_empty() {
        return Err("Enter a title.");
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err("Keep the title under 120 characters.");
    }
    Ok(title.to_string())
}

fn is_long_press(start_ms: f64, end_ms: f64) -> bool {
    end_ms - start_ms >= LONG_PRESS_MS
}

fn clamped_desktop_menu_top(row_top: f64, viewport_height: f64) -> f64 {
    let max_top =
        (viewport_height - DESKTOP_MENU_MAX_HEIGHT - DESKTOP_MENU_GUTTER).max(DESKTOP_MENU_GUTTER);
    row_top.clamp(DESKTOP_MENU_GUTTER, max_top)
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

#[cfg(target_arch = "wasm32")]
fn browser_now_ms() -> Option<f64> {
    Some(js_sys::Date::now())
}

#[cfg(not(target_arch = "wasm32"))]
fn browser_now_ms() -> Option<f64> {
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConversationActionView {
    Actions,
    Rename,
    ConfirmDelete,
}

#[component]
pub(super) fn ConversationRow(
    id: String,
    title: Option<String>,
    created_at: String,
    snippets: Vec<String>,
    is_active: bool,
    menu_open: bool,
    action_error: Option<String>,
    on_select: EventHandler<MouseEvent>,
    on_menu_open: EventHandler<String>,
    on_menu_close: EventHandler<()>,
    on_delete: EventHandler<String>,
    on_rename: EventHandler<(String, String)>,
) -> Element {
    let initial_title = title.clone().unwrap_or_default();
    let display_title = conv_title(&title).to_string();
    let row_dom_id = format!("conversation-row-{id}");
    let mut action_view = use_signal(|| ConversationActionView::Actions);
    let mut draft_title = use_signal(move || initial_title.clone());
    let mut validation_error = use_signal(|| None::<&'static str>);
    let mut touch_started_at = use_signal(|| None::<f64>);
    let mut suppress_next_select = use_signal(|| false);
    let mut menu_position_style = use_signal(String::new);

    let cls = if is_active {
        "conv-item conv-item-active"
    } else {
        "conv-item"
    };
    let open_actions = use_callback({
        let id = id.clone();
        let row_dom_id = row_dom_id.clone();
        let existing_title = title.clone().unwrap_or_default();
        move |_: ()| {
            action_view.set(ConversationActionView::Actions);
            draft_title.set(existing_title.clone());
            validation_error.set(None);
            menu_position_style.set(desktop_menu_style(&row_dom_id));
            on_menu_open.call(id.clone());
        }
    });
    let close_actions = use_callback(move |_: ()| {
        touch_started_at.set(None);
        suppress_next_select.set(false);
        validation_error.set(None);
        on_menu_close.call(());
    });

    rsx! {
        div {
            id: "{row_dom_id}",
            class: "conv-item-row",
            button {
                class: "{cls}",
                r#type: "button",
                title: "Open chat. Press and hold or right-click for options.",
                onclick: move |evt| {
                    // A long touch normally synthesizes a click on release. Consume that one click
                    // so opening the action sheet cannot also open/collapse the conversation.
                    if suppress_next_select() || menu_open {
                        suppress_next_select.set(false);
                        evt.prevent_default();
                        evt.stop_propagation();
                    } else {
                        on_select.call(evt);
                    }
                },
                oncontextmenu: move |evt| {
                    evt.prevent_default();
                    evt.stop_propagation();
                    touch_started_at.set(None);
                    open_actions.call(());
                },
                ontouchstart: move |_| touch_started_at.set(browser_now_ms()),
                // Any movement means the gesture is a scroll, not a hold.
                ontouchmove: move |_| touch_started_at.set(None),
                ontouchcancel: move |_| touch_started_at.set(None),
                ontouchend: move |_| {
                    let started = touch_started_at();
                    touch_started_at.set(None);
                    if let (Some(start), Some(end)) = (started, browser_now_ms())
                        && is_long_press(start, end)
                    {
                        suppress_next_select.set(true);
                        open_actions.call(());
                    }
                },
                div {
                    class: "conv-item-title",
                    "{display_title}"
                }
                div {
                    class: "conv-item-time",
                    "{relative_time(&created_at)}"
                }
                for snippet in snippets.iter() {
                    div {
                        class: "search-result-snippet",
                        "{snippet}"
                    }
                }
            }
            if menu_open {
                div {
                    class: "conv-menu-backdrop",
                    style: "{menu_position_style}",
                    onclick: move |_| close_actions.call(()),
                    // On desktop the sidebar remains exposed, so another row receives its own
                    // context-menu event. Everywhere else on the backdrop, suppress the browser's
                    // native menu and dismiss ours.
                    oncontextmenu: move |evt| {
                        evt.prevent_default();
                        evt.stop_propagation();
                        close_actions.call(());
                    },
                    div {
                        class: "conv-menu",
                        onclick: move |evt: MouseEvent| evt.stop_propagation(),
                        if let Some(message) = action_error.clone() {
                            p { class: "conv-menu-error server", "{message}" }
                        }
                        match action_view() {
                            ConversationActionView::Actions => rsx! {
                                p { class: "conv-menu-label", "{display_title}" }
                                p { class: "conv-menu-note", "Choose what to do with this chat." }
                                div {
                                    class: "conv-menu-actions stacked",
                                    button {
                                        class: "conv-menu-btn",
                                        r#type: "button",
                                        onclick: move |_| {
                                            validation_error.set(None);
                                            action_view.set(ConversationActionView::Rename);
                                        },
                                        "Rename"
                                    }
                                    button {
                                        class: "conv-menu-btn danger",
                                        r#type: "button",
                                        onclick: move |_| {
                                            validation_error.set(None);
                                            action_view.set(ConversationActionView::ConfirmDelete);
                                        },
                                        "Delete"
                                    }
                                    button {
                                        class: "conv-menu-btn",
                                        r#type: "button",
                                        onclick: move |_| close_actions.call(()),
                                        "Cancel"
                                    }
                                }
                            },
                            ConversationActionView::Rename => rsx! {
                                form {
                                    onsubmit: {
                                        let id = id.clone();
                                        move |evt| {
                                            evt.prevent_default();
                                            match normalized_title(draft_title.read().as_str()) {
                                                Ok(new_title) => {
                                                    suppress_next_select.set(false);
                                                    validation_error.set(None);
                                                    on_rename.call((id.clone(), new_title));
                                                }
                                                Err(message) => validation_error.set(Some(message)),
                                            }
                                        }
                                    },
                                    p { class: "conv-menu-label", "Rename chat" }
                                    input {
                                        class: "conv-menu-input",
                                        r#type: "text",
                                        maxlength: "{MAX_TITLE_CHARS}",
                                        value: "{draft_title}",
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
                                                action_view.set(ConversationActionView::Actions);
                                            },
                                            "Back"
                                        }
                                    }
                                }
                            },
                            ConversationActionView::ConfirmDelete => rsx! {
                                p { class: "conv-menu-label", "Delete permanently?" }
                                p { class: "conv-menu-note", "Removes it from disk. There is no undo." }
                                div {
                                    class: "conv-menu-actions",
                                    button {
                                        class: "conv-menu-btn danger",
                                        r#type: "button",
                                        onclick: {
                                            let id = id.clone();
                                            move |_| {
                                                suppress_next_select.set(false);
                                                on_delete.call(id.clone());
                                            }
                                        },
                                        "Delete"
                                    }
                                    button {
                                        class: "conv-menu-btn",
                                        r#type: "button",
                                        onclick: move |_| {
                                            action_view.set(ConversationActionView::Actions);
                                        },
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
}

#[cfg(test)]
mod tests {
    use super::{
        DESKTOP_MENU_GUTTER, LONG_PRESS_MS, clamped_desktop_menu_top, conv_title, is_long_press,
        normalized_title,
    };

    #[test]
    fn empty_titles_are_untitled() {
        assert_eq!(conv_title(&Some("A title".into())), "A title");
        assert_eq!(conv_title(&Some(String::new())), "Untitled");
        assert_eq!(conv_title(&None), "Untitled");
        assert_eq!(conv_title(&Some("  ".into())), "  ");
    }

    #[test]
    fn long_press_threshold_keeps_short_taps_as_selection() {
        assert!(!is_long_press(1_000.0, 1_000.0 + LONG_PRESS_MS - 1.0));
        assert!(is_long_press(1_000.0, 1_000.0 + LONG_PRESS_MS));
    }

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
    fn desktop_action_sheet_stays_inside_the_viewport() {
        assert_eq!(clamped_desktop_menu_top(100.0, 900.0), 100.0);
        assert_eq!(clamped_desktop_menu_top(-20.0, 900.0), DESKTOP_MENU_GUTTER);
        assert_eq!(clamped_desktop_menu_top(850.0, 900.0), 652.0);
    }
}
