use dioxus::prelude::*;

use chat_client_contract::{ConvHeader, ConversationSearchResponse, ConversationSearchResult};

use crate::components::mcp_panel::McpPanel;
use crate::icons::IconChevronLeft;

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

async fn fetch_conversations(api_base: String) -> Result<Vec<ConvHeader>, String> {
    let url = format!("{api_base}/api/conversations");
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    let headers: Vec<ConvHeader> = resp
        .json()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    Ok(headers)
}

/// A delete "succeeded" when the daemon answered 2xx, or 404 — the row was already gone, which
/// is the outcome the caller wanted.
fn delete_accepted(status: u16) -> bool {
    (200..300).contains(&status) || status == 404
}

/// `DELETE /api/conversations/{id}` — a real delete: the daemon removes the log from disk.
///
/// A 404 counts as success. The only thing reporting it would tell the user is something they cannot
/// act on ("it was already gone"), and the outcome they asked for — that row not existing — holds
/// either way.
async fn delete_conversation(api_base: &str, id: &str) -> Result<(), String> {
    let url = format!("{api_base}/api/conversations/{id}");
    let resp = reqwest::Client::new()
        .delete(&url)
        .send()
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if delete_accepted(resp.status().as_u16()) {
        Ok(())
    } else {
        Err(format!("Delete failed: HTTP {}", resp.status().as_u16()))
    }
}

/// `PATCH /api/conversations/{id}` — update only the conversation's display title.
async fn rename_conversation(api_base: &str, id: &str, title: &str) -> Result<(), String> {
    let url = format!("{api_base}/api/conversations/{id}");
    let resp = reqwest::Client::new()
        .patch(&url)
        .json(&serde_json::json!({ "title": title }))
        .send()
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if (200..300).contains(&resp.status().as_u16()) {
        Ok(())
    } else {
        Err(format!("Rename failed: HTTP {}", resp.status().as_u16()))
    }
}

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

async fn fetch_search_results(
    api_base: String,
    query: String,
) -> Result<Vec<ConversationSearchResult>, String> {
    let url = format!("{api_base}/api/conversations/search");
    let resp = reqwest::Client::new()
        .get(&url)
        .query(&[("q", query.as_str())])
        .send()
        .await
        .map_err(|e| format!("Search failed: {e}"))?;
    let body: ConversationSearchResponse = resp
        .json()
        .await
        .map_err(|e| format!("Bad search response: {e}"))?;
    Ok(body.results)
}

/// A short line for the conversation-list fetch, not the raw reqwest string.
///
/// The raw form (`Error: Bad response: error decoding response body`) sat in the sidebar forever
/// with no way to try again. Keep the cause class, drop the decoder dump.
fn conversations_error_label(err: &str) -> &'static str {
    if err.starts_with("Failed to reach daemon") {
        "Could not reach the daemon."
    } else if err.starts_with("Bad response") {
        "Could not read the conversation list."
    } else {
        "Could not load conversations."
    }
}

/// Close the sidebar after the user picks something in it — but only where it is an overlay
/// sitting on top of the chat. On a phone the sidebar covers the whole content area, so leaving it
/// open after a selection hides the very conversation you just chose and forces a second tap. On a
/// wide screen it is a persistent side panel, and closing it would take the list away for no reason.
fn collapse_after_pick(mut collapsed: Signal<bool>) {
    if crate::is_narrow_viewport() {
        collapsed.set(true);
    }
}

fn relative_time(iso: &str) -> String {
    let parsed = chrono::DateTime::parse_from_rfc3339(iso)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(&format!("{iso}Z")))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(iso, "%Y-%m-%dT%H:%M:%S%.f")
                .map(|naive| naive.and_utc().into())
        });
    let dt = match parsed {
        Ok(dt) => dt,
        Err(_) => return String::new(),
    };
    let now = chrono::Utc::now();
    let dur = now.signed_duration_since(dt);
    if dur.num_seconds() < 0 {
        return "just now".into();
    }
    let secs = dur.num_seconds() as u64;
    if secs < 60 {
        return format!("{}s ago", secs);
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{}m ago", mins);
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{}h ago", hours);
    }
    let days = hours / 24;
    if days < 30 {
        return format!("{}d ago", days);
    }
    dt.format("%b %d").to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        DESKTOP_MENU_GUTTER, LONG_PRESS_MS, clamped_desktop_menu_top, conv_title,
        conversations_error_label, delete_accepted, is_long_press, normalized_title, relative_time,
    };

    fn ago(dur: chrono::Duration) -> String {
        (chrono::Utc::now() - dur).to_rfc3339()
    }

    /// Unparseable input renders as nothing rather than a garbage label or a panic.
    #[test]
    fn unparseable_is_empty() {
        assert_eq!(relative_time("not-a-date"), "");
    }

    /// A timestamp from the future (clock skew, a fast daemon clock) reads as "just now" — never a
    /// negative age.
    #[test]
    fn future_reads_as_just_now() {
        assert_eq!(
            relative_time(&ago(-chrono::Duration::minutes(1))),
            "just now"
        );
    }

    /// The age bands choose their unit, matching what a conversation list should say at each
    /// distance.
    #[test]
    fn age_bands_choose_the_unit() {
        assert_eq!(
            relative_time(&ago(chrono::Duration::seconds(12))),
            "12s ago"
        );
        assert_eq!(relative_time(&ago(chrono::Duration::minutes(5))), "5m ago");
        assert_eq!(relative_time(&ago(chrono::Duration::hours(3))), "3h ago");
        assert_eq!(relative_time(&ago(chrono::Duration::days(2))), "2d ago");
    }

    /// Past a month, a relative count stops being readable; the row shows the calendar date
    /// instead. Fixed input so the expectation is exact, not derived from the same function.
    #[test]
    fn past_a_month_shows_the_date() {
        assert_eq!(relative_time("2000-01-15T10:00:00Z"), "Jan 15");
    }

    /// A timestamp without an offset (as older daemon versions emitted) is still understood — the
    /// append-`Z` fallback treats it as UTC rather than dropping the row's time label.
    #[test]
    fn offsetless_timestamp_is_treated_as_utc() {
        let raw = (chrono::Utc::now() - chrono::Duration::minutes(7))
            .format("%Y-%m-%dT%H:%M:%S%.f")
            .to_string();
        assert_eq!(relative_time(&raw), "7m ago");
    }

    /// The band boundaries are exact: 60s is a minute ago, not 60 seconds ago — the `<` comparisons
    /// must be strict or the unit drifts by exactly one.
    #[test]
    fn band_boundaries_fall_up() {
        let at = |secs: i64| (chrono::Utc::now() - chrono::Duration::seconds(secs)).to_rfc3339();
        assert_eq!(relative_time(&at(60)), "1m ago");
        assert_eq!(relative_time(&at(3600)), "1h ago");
        assert_eq!(relative_time(&at(86_400)), "1d ago");
    }

    /// Past a month the row shows the calendar date — the `days < 30` is strict, so exactly 30 days
    /// is already "months ago" and gets the date form. Expected date is derived from the same
    /// instant so the assertion is about the *form chosen*, not a fixed calendar day.
    #[test]
    fn exactly_thirty_days_shows_the_date() {
        let raw = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let expected = chrono::DateTime::parse_from_rfc3339(&raw)
            .unwrap()
            .format("%b %d")
            .to_string();
        assert_eq!(relative_time(&raw), expected);
    }

    /// A sub-second delta reads as zero seconds ago, not "just now" — the future check is strict
    /// (`< 0`, not `<= 0`), so a just-created row ages from 0s.
    #[test]
    fn sub_second_ages_from_zero() {
        let raw = (chrono::Utc::now() - chrono::Duration::milliseconds(500)).to_rfc3339();
        assert_eq!(relative_time(&raw), "0s ago");
    }

    /// `delete_accepted` treats 2xx and 404 as success, everything else as failure.
    #[test]
    fn delete_accepts_2xx_and_404() {
        for ok in [200, 204, 299, 404] {
            assert!(delete_accepted(ok), "{ok} should delete cleanly");
        }
        for no in [300, 403, 500] {
            assert!(
                !delete_accepted(no),
                "{no} should surface as a delete failure"
            );
        }
    }

    /// Fetch failures render a short human line, never the reqwest decoder dump.
    #[test]
    fn conversation_fetch_errors_are_short() {
        assert_eq!(
            conversations_error_label("Failed to reach daemon: connection refused"),
            "Could not reach the daemon."
        );
        assert_eq!(
            conversations_error_label("Bad response: error decoding response body"),
            "Could not read the conversation list."
        );
        assert_eq!(
            conversations_error_label("something else"),
            "Could not load conversations."
        );
    }

    /// An unnamed conversation reads as "Untitled" in both the conversation list and search results.
    #[test]
    fn empty_titles_are_untitled() {
        assert_eq!(conv_title(&Some("A title".into())), "A title");
        assert_eq!(conv_title(&Some(String::new())), "Untitled");
        assert_eq!(conv_title(&None), "Untitled");
        // Whitespace is not emptiness — matches the `!is_empty()` guard.
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

#[component]
pub fn Sidebar(
    api_base: String,
    active_conv_id: Signal<Option<String>>,
    collapsed: Signal<bool>,
    /// Bumped whenever "New Chat" is pressed. See the button below for why a counter and not a bool.
    new_chat_nonce: Signal<u64>,
) -> Element {
    let mut new_chat_nonce = new_chat_nonce;
    // `mut` for `restart()` after a delete, which is wasm-only — on native that writer is cfg'd out
    // and the binding merely looks immutable.
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut conversations = use_resource({
        let base = api_base.clone();
        move || {
            let _ = active_conv_id.read();
            fetch_conversations(base.clone())
        }
    });

    let mut search_query = use_signal(String::new);
    // Which row's action sheet is open, by conversation id. Held here rather than per row so
    // opening one closes another — two open sheets at once is just clutter.
    let mut menu_for = use_signal(|| None::<String>);
    let mut action_error = use_signal(|| None::<String>);

    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut search_results = use_resource({
        let base = api_base.clone();
        move || {
            let q = search_query.read().trim().to_string();
            let base = base.clone(); // fresh per-call clone; the async block below consumes it
            async move {
                if q.is_empty() {
                    None
                } else {
                    Some(fetch_search_results(base, q).await)
                }
            }
        }
    });

    let open_menu = use_callback(move |id: String| {
        menu_for.set(Some(id));
        action_error.set(None);
    });
    let close_menu = use_callback(move |_: ()| {
        menu_for.set(None);
        action_error.set(None);
    });

    let toggle = move |_| collapsed.set(!collapsed());

    let delete_conv = {
        let base = api_base.clone();
        use_callback(move |id: String| {
            let base = base.clone();
            #[cfg(target_arch = "wasm32")]
            wasm_bindgen_futures::spawn_local(async move {
                match delete_conversation(&base, &id).await {
                    Ok(()) => {
                        menu_for.set(None);
                        action_error.set(None);
                        // Viewing the one just deleted? Fall back to a fresh chat rather than
                        // leaving the pane showing a conversation that no longer exists.
                        if active_conv_id.read().as_deref() == Some(id.as_str()) {
                            active_conv_id.set(None);
                        }
                        conversations.restart();
                        search_results.restart();
                    }
                    Err(e) => action_error.set(Some(e)),
                }
            });
            #[cfg(not(target_arch = "wasm32"))]
            {
                let _ = (base, id);
            }
        })
    };

    let rename_conv = {
        let base = api_base.clone();
        use_callback(move |(id, title): (String, String)| {
            let base = base.clone();
            #[cfg(target_arch = "wasm32")]
            wasm_bindgen_futures::spawn_local(async move {
                match rename_conversation(&base, &id, &title).await {
                    Ok(()) => {
                        menu_for.set(None);
                        action_error.set(None);
                        conversations.restart();
                        search_results.restart();
                    }
                    Err(e) => action_error.set(Some(e)),
                }
            });
            #[cfg(not(target_arch = "wasm32"))]
            {
                let _ = (base, id, title);
            }
        })
    };

    // Collapsed renders *nothing* — not a slim rail. A rail spent a vertical strip of every screen
    // on one button, which on a phone is a chunk of the conversation's width. The button that brings
    // this back lives in the app header (see main.rs), where it costs no layout width at all.
    if collapsed() {
        return rsx! {};
    }

    rsx! {
        div {
            class: "sidebar",
            div {
                class: "sidebar-header",
                button {
                    class: "sidebar-new-chat-btn",
                    onclick: move |_| {
                        // Announce the *request* rather than infer it from state. This used to be
                        // only `if active_conv_id.is_some() { set(None) }`, on the premise that
                        // `None` already means "fresh and empty" — true until incognito, whose
                        // session deliberately never becomes an `active_conv_id`. New Chat then
                        // no-opped in exactly the case where clearing mattered most, leaving the
                        // private transcript on screen.
                        //
                        // A counter, not a bool: pressing New Chat twice has to register twice, and
                        // a flag that is already `true` cannot say "again".
                        new_chat_nonce += 1;
                        if active_conv_id.read().is_some() {
                            active_conv_id.set(None);
                        }
                        collapse_after_pick(collapsed);
                    },
                    "New Chat"
                }
                button {
                    class: "sidebar-collapse-btn",
                    onclick: toggle,
                    title: "Collapse sidebar",
                    IconChevronLeft {}
                }
            }
            div {
                class: "sidebar-search",
                input {
                    class: "sidebar-search-input",
                    r#type: "search",
                    placeholder: "Search conversations...",
                    value: "{search_query}",
                    oninput: move |evt| search_query.set(evt.value()),
                }
            }
            p {
                class: "sidebar-gesture-hint",
                "Press and hold a chat for rename or delete."
            }
            div {
                class: "sidebar-list",
                if !search_query.read().trim().is_empty() {
                    match &*search_results.read() {
                        Some(Some(Ok(list))) => {
                            if list.is_empty() {
                                rsx! {
                                    p { class: "sidebar-empty", "No results." }
                                }
                            } else {
                                rsx! {
                                    for result in list {
                                        ConversationRow {
                                            key: "{result.conversation_id}",
                                            id: result.conversation_id.clone(),
                                            title: result.title.clone(),
                                            created_at: result.created_at.clone(),
                                            snippets: result.matches.iter()
                                                .map(|item| item.content_snippet.clone())
                                                .collect::<Vec<_>>(),
                                            is_active: active_conv_id.read().as_deref() == Some(&result.conversation_id),
                                            menu_open: menu_for.read().as_deref() == Some(&result.conversation_id),
                                            action_error: action_error(),
                                            on_select: {
                                                let mut active = active_conv_id;
                                                let id = result.conversation_id.clone();
                                                move |_| {
                                                    menu_for.set(None);
                                                    active.set(Some(id.clone()));
                                                    collapse_after_pick(collapsed);
                                                }
                                            },
                                            on_menu_open: open_menu,
                                            on_menu_close: close_menu,
                                            on_delete: delete_conv,
                                            on_rename: rename_conv,
                                        }
                                    }
                                }
                            }
                        }
                        Some(Some(Err(e))) => rsx! {
                            p { class: "sidebar-empty", "Error: {e}" }
                        },
                        Some(None) | None => rsx! {
                            p { class: "sidebar-empty", "Searching..." }
                        },
                    }
                } else {
                    match &*conversations.read() {
                        Some(Ok(list)) => {
                            if list.is_empty() {
                                rsx! {
                                    p {
                                        class: "sidebar-empty",
                                        "No conversations yet."
                                    }
                                }
                            } else {
                                rsx! {
                                    for conv in list {
                                        ConversationRow {
                                            key: "{conv.id}",
                                            id: conv.id.clone(),
                                            title: conv.title.clone(),
                                            created_at: conv.created_at.clone(),
                                            snippets: Vec::new(),
                                            is_active: active_conv_id.read().as_deref() == Some(&conv.id),
                                            menu_open: menu_for.read().as_deref() == Some(&conv.id),
                                            action_error: action_error(),
                                            on_select: {
                                                let mut active = active_conv_id;
                                                let id = conv.id.clone();
                                                move |_| {
                                                    menu_for.set(None);
                                                    active.set(Some(id.clone()));
                                                    collapse_after_pick(collapsed);
                                                }
                                            },
                                            on_menu_open: open_menu,
                                            on_menu_close: close_menu,
                                            on_delete: delete_conv,
                                            on_rename: rename_conv,
                                        }
                                    }
                                }
                            }
                        }
                        Some(Err(e)) => rsx! {
                            div {
                                class: "sidebar-error",
                                p {
                                    class: "sidebar-empty",
                                    "{conversations_error_label(e)}"
                                }
                                button {
                                    class: "sidebar-retry-btn",
                                    r#type: "button",
                                    onclick: move |_| conversations.restart(),
                                    "Retry"
                                }
                            }
                        },
                        None => rsx! {
                            p {
                                class: "sidebar-empty",
                                "Loading..."
                            }
                        },
                    }
                }
            }
            div {
                class: "sidebar-footer",
                McpPanel { api_base: api_base.clone() }
            }
        }
    }
}

/// The row's title, or "Untitled" when it is missing or empty. Shared by the conversation row and
/// the search-result row so the two lists cannot disagree about what an unnamed conversation is
/// called. Whitespace-only titles pass through — the guard is `!is_empty()`, not `!trim().is_empty()`.
fn conv_title(title: &Option<String>) -> &str {
    match title {
        Some(t) if !t.is_empty() => t.as_str(),
        _ => "Untitled",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConversationActionView {
    Actions,
    Rename,
    ConfirmDelete,
}

#[component]
fn ConversationRow(
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
