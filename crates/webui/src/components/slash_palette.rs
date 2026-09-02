//! The slash-command palette that pops up above the input, and the inline ghost completion.
//!
//! Behaviour is the TUI's, because it *is* the TUI's: matching, the progressive Tab fill and the
//! ghost remainder all come from `liberado_commands` (`filter_commands`, `complete_commands`,
//! `ghost_suffix`), which the TUI's `handlers/input.rs` and `render/slash_palette.rs` call too.
//! Nothing about which commands exist or how a prefix resolves is decided here — this file is only
//! the presentation, so the two surfaces cannot drift on what `/th` means.
//!
//! Deliberately *not* the shared [`Picker`](crate::components::picker::Picker): a picker is a modal
//! that takes over the screen and owns the keyboard. This is the opposite — it hovers over the
//! conversation, must not steal focus from the textarea (you are still typing into it), and closes
//! itself as the query stops matching. Same reason the TUI draws it separately from its own modals.

use dioxus::prelude::*;

use liberado_commands::{CommandSpec, filter_commands};

/// Rows visible at once before the list starts scrolling with the selection. Matches the TUI's
/// `MAX_VISIBLE`, so a given query shows the same window on both surfaces.
pub const MAX_VISIBLE: usize = 8;

/// The window of matches to show, and the selection's index within it.
///
/// Scrolls only as far as it must to keep the selection on screen — the same arithmetic the TUI's
/// palette does, kept here rather than in the component so it can be reasoned about on its own.
fn visible_window(total: usize, selected: usize) -> (usize, usize) {
    let visible = total.min(MAX_VISIBLE);
    let mut start = selected.saturating_sub(visible.saturating_sub(1));
    if start + visible > total {
        start = total.saturating_sub(visible);
    }
    (start, (start + visible).min(total))
}

/// The matches for `input`, or empty when it is not a slash query.
pub fn matches_for(input: &str) -> Vec<&'static CommandSpec> {
    filter_commands(input)
}

#[component]
pub fn SlashPalette(
    /// The raw input text. The palette derives everything from it, so it cannot show a list that
    /// disagrees with what completion would do.
    input: String,
    selected: usize,
    /// Tapping a row runs that exact command — the phone equivalent of selecting it and pressing
    /// Enter, and the reason this is a list of buttons rather than styled text.
    on_run: EventHandler<String>,
) -> Element {
    let matches = matches_for(&input);
    if matches.is_empty() {
        return rsx! {};
    }
    let total = matches.len();
    let selected = selected.min(total - 1);
    let (start, end) = visible_window(total, selected);

    let hint = if total > MAX_VISIBLE {
        format!(
            "{}/{} \u{00B7} Tap run \u{00B7} Tab fill \u{00B7} Enter run",
            selected + 1,
            total
        )
    } else {
        "Tap run \u{00B7} Tab fill \u{00B7} Enter run".to_string()
    };

    rsx! {
        div {
            class: "slash-palette",
            // Keep the textarea focused: a mousedown here would blur it, and the palette exists to
            // help someone who is mid-sentence. `onclick` on the rows still fires.
            onmousedown: move |e| e.prevent_default(),
            div {
                class: "slash-palette-list",
                for (offset, spec) in matches[start..end].iter().enumerate() {
                    {
                        let idx = start + offset;
                        let command = spec.insert.to_string();
                        let cls = if idx == selected {
                            "slash-row selected"
                        } else {
                            "slash-row"
                        };
                        rsx! {
                            button {
                                key: "{spec.name}",
                                class: "{cls}",
                                r#type: "button",
                                onclick: move |_| on_run.call(command.clone()),
                                span { class: "slash-row-name", "{spec.name}" }
                                span { class: "slash-row-desc", "{spec.description}" }
                            }
                        }
                    }
                }
            }
            div { class: "slash-palette-hint", "{hint}" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window must keep the selection visible without scrolling further than necessary — the
    /// property the arithmetic exists for, and the one that is easy to get subtly wrong at the ends.
    #[test]
    fn window_follows_the_selection_and_stays_in_bounds() {
        // Short list: no scrolling at all.
        assert_eq!(visible_window(3, 0), (0, 3));
        assert_eq!(visible_window(3, 2), (0, 3));

        // Long list, selection near the top: show the first page.
        assert_eq!(visible_window(20, 0), (0, MAX_VISIBLE));
        assert_eq!(visible_window(20, MAX_VISIBLE - 1), (0, MAX_VISIBLE));

        // Selection past the first page: scroll by exactly enough to include it.
        let (start, end) = visible_window(20, MAX_VISIBLE);
        assert_eq!(end - start, MAX_VISIBLE);
        assert_eq!(end, MAX_VISIBLE + 1);

        // Last item: the window ends at the list end, never past it.
        let (start, end) = visible_window(20, 19);
        assert_eq!(end, 20);
        assert_eq!(end - start, MAX_VISIBLE);

        // A small list with the window sliding right: the first visible row only advances once the
        // selection passes the head of the window.
        let (start, end) = visible_window(15, 9);
        assert_eq!((start, end), (2, 10));
        assert_eq!(end - start, MAX_VISIBLE);
    }

    #[test]
    fn an_empty_list_produces_an_empty_window_rather_than_panicking() {
        assert_eq!(visible_window(0, 0), (0, 0));
    }

    /// Guards the claim in the module docs: the palette's list is the shared catalog's list.
    #[test]
    fn matches_come_from_the_shared_catalog() {
        assert!(matches_for("hello").is_empty(), "not a slash query");
        let m = matches_for("/hel");
        assert!(m.iter().any(|s| s.name == "/help"));
        assert_eq!(m, liberado_commands::filter_commands("/hel"));
    }
}
