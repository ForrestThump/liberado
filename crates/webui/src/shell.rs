//! Phone layout helpers and the header controls.
//!
//! Split from `main.rs` so that file stays inside the module-health function boundary.

use dioxus::prelude::*;

use crate::components::grants_popover::ToolsPopover;
use crate::components::incognito::IncognitoToggle;
use crate::components::profile_browser::ProfileChip;
use crate::components::profile_surface::profile_chip_visible;
use crate::icons::IconMenu;

/// Same cutoff as `@media (max-width: 768px)` in `styles/main.css`.
///
/// Inclusive on purpose: the CSS query matches a viewport of exactly 768px, and the drawer
/// signal has to agree or the rail stays in the document while the stylesheet treats it as a phone.
pub(crate) const NARROW_LAYOUT_MAX_PX: f64 = 768.0;

/// `matchMedia` query kept beside [`NARROW_LAYOUT_MAX_PX`].
pub(crate) const NARROW_LAYOUT_MEDIA_QUERY: &str = "(max-width: 768px)";

/// `width_px` is phone-width when it is inside the CSS phone query.
pub(crate) fn viewport_is_narrow(width_px: f64) -> bool {
    width_px <= NARROW_LAYOUT_MAX_PX
}

/// Whether the viewport is phone-width, at the same breakpoint `main.css` uses for its layout
/// media query. Kept in one place because two behaviours key off it: the sidebar starts collapsed
/// here, and it re-collapses itself after you pick a conversation — but only where it is an
/// overlay covering the chat. On a wide screen it is a side panel, and closing it on every
/// selection would just take the conversation list away from you.
///
/// A one-shot sample. The App keeps a signal that tracks resize via matchMedia.
pub(crate) fn is_narrow_viewport() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.inner_width().ok())
            .and_then(|v| v.as_f64())
            .map(viewport_is_narrow)
            .unwrap_or(false)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        false
    }
}

/// Phone drawer is open only while the viewport is narrow and the sidebar panel is expanded.
///
/// Desktop collapse is an in-flow icon rail, so this stays false there even when the panel is open.
pub(crate) fn sidebar_drawer_open(narrow: bool, collapsed: bool) -> bool {
    narrow && !collapsed
}

/// Layout class for the sidebar + chat row. `drawer-open` is the stylesheet's signal that the
/// phone panel is a fixed overlay instead of a column.
pub(crate) fn app_layout_class(drawer_open: bool) -> &'static str {
    if drawer_open {
        "app-layout grok-layout drawer-open"
    } else {
        "app-layout grok-layout"
    }
}

/// Keep `narrow` in sync with the live viewport.
#[cfg(target_arch = "wasm32")]
pub(crate) fn watch_viewport_width(mut narrow: Signal<bool>) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    let Some(window) = web_sys::window() else {
        return;
    };
    let handler = Closure::<dyn FnMut()>::new(move || {
        let now = is_narrow_viewport();
        if narrow() != now {
            narrow.set(now);
        }
    });
    if let Ok(Some(mql)) = window.match_media(NARROW_LAYOUT_MEDIA_QUERY) {
        let _ = mql.add_event_listener_with_callback("change", handler.as_ref().unchecked_ref());
    } else {
        let _ = window.add_event_listener_with_callback("resize", handler.as_ref().unchecked_ref());
    }
    handler.forget();
}

// ── Grok-style top bar ─────────────────────────────────────────────────────

/// CSS class on the desktop-only shell header (`display: none` inside the phone query).
pub(crate) const DESKTOP_APP_HEADER_CLASS: &str = "app-header-desktop-root";

/// CSS class on in-flow phone headers that scroll with page content.
pub(crate) const PHONE_SCROLL_APP_HEADER_CLASS: &str = "app-header-in-scroll";

/// Marker pair for tests that assert phone scroll-header rules in `main.css`.
pub(crate) const PHONE_SCROLL_HEADER_CSS_START: &str = "/* phone-scroll-header */";
pub(crate) const PHONE_SCROLL_HEADER_CSS_END: &str = "/* phone-scroll-header-end */";

/// Liberado top bar: brand, optional project chip, profile, Tools, incognito.
#[component]
pub(crate) fn GrokAppHeader(
    /// Extra classes after `app-header grok-header` (desktop root or in-scroll variant).
    shell_class: &'static str,
    drawer_open: bool,
    mut sidebar_collapsed: Signal<bool>,
    active_project: Signal<Option<String>>,
    active_profile: Signal<Option<String>>,
    active_surface: Signal<Option<chat_client_contract::SurfaceMode>>,
    on_profile_open: EventHandler<()>,
    incognito: Signal<bool>,
    api_base: String,
    active_conv_id: Signal<Option<String>>,
    tools_open: Signal<bool>,
) -> Element {
    rsx! {
        header {
            class: "app-header grok-header {shell_class}",
            div {
                class: "grok-header-inner",
                button {
                    class: "sidebar-menu-btn",
                    r#type: "button",
                    title: if drawer_open { "Close sidebar" } else { "Open sidebar" },
                    "aria-label": if drawer_open { "Close sidebar" } else { "Open sidebar" },
                    "aria-expanded": "{drawer_open}",
                    "aria-controls": "sidebar",
                    onclick: move |_| sidebar_collapsed.set(!sidebar_collapsed()),
                    IconMenu {}
                }
                span { class: "brand grok-header-brand", "Liberado" }
                if let Some(name) = active_project() {
                    span { class: "grok-header-project", "{name}" }
                }
                HeaderProfileChip {
                    active_profile,
                    active_surface,
                    on_open: on_profile_open,
                }
                HeaderRightSide {
                    incognito,
                    api_base,
                    active_conv_id,
                    tools_open,
                }
            }
        }
    }
}

/// Fixed menu control on a phone. The in-flow header scrolls away; this stays reachable.
#[component]
pub(crate) fn PhoneMenuFab(drawer_open: bool, mut sidebar_collapsed: Signal<bool>) -> Element {
    rsx! {
        button {
            class: "phone-menu-fab",
            r#type: "button",
            title: if drawer_open { "Close sidebar" } else { "Open sidebar" },
            "aria-label": if drawer_open { "Close sidebar" } else { "Open sidebar" },
            "aria-expanded": "{drawer_open}",
            "aria-controls": "sidebar",
            onclick: move |_| sidebar_collapsed.set(!sidebar_collapsed()),
            IconMenu {}
        }
    }
}

/// Top-right region: the read-only Tools list, then the incognito pill. Agent selection stays
/// on the sidebar shelf. Tools lives here so Back can close it with the other header layers.
#[component]
pub(crate) fn HeaderRightSide(
    incognito: Signal<bool>,
    api_base: String,
    active_conv_id: Signal<Option<String>>,
    tools_open: Signal<bool>,
) -> Element {
    rsx! {
        div {
            class: "header-right-cluster",
            ToolsPopover {
                api_base,
                active_conv_id,
                open: tools_open,
            }
            IncognitoToggle { on: incognito }
        }
    }
}

/// Header profile control. An Agent surface renders nothing — the chip is a Chats concept.
#[component]
pub(crate) fn HeaderProfileChip(
    active_profile: Signal<Option<String>>,
    active_surface: Signal<Option<chat_client_contract::SurfaceMode>>,
    on_open: EventHandler<()>,
) -> Element {
    if !profile_chip_visible(active_surface()) {
        return rsx! {};
    }
    rsx! {
        ProfileChip { active_profile, on_open }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The host build has no browser: these are the stub branches, pinned so the wasm and host
    // halves cannot drift in intent.

    /// No browser means no viewport; the host build must not claim the layout is phone-width.
    #[test]
    fn host_viewport_is_never_narrow() {
        assert!(!is_narrow_viewport());
    }

    /// 768px is inside `@media (max-width: 768px)`. One pixel wider is the desktop rail.
    #[test]
    fn viewport_breakpoint_matches_the_phone_media_query() {
        assert_eq!(NARROW_LAYOUT_MEDIA_QUERY, "(max-width: 768px)");
        assert!(viewport_is_narrow(0.0));
        assert!(viewport_is_narrow(390.0));
        assert!(viewport_is_narrow(NARROW_LAYOUT_MAX_PX));
        assert!(!viewport_is_narrow(NARROW_LAYOUT_MAX_PX + 1.0));
    }

    /// The drawer covers the chat only on a phone, and only while the panel is expanded.
    /// A collapsed phone and any desktop width stay closed so the chat keeps the full row.
    #[test]
    fn drawer_opens_only_for_an_expanded_phone_sidebar() {
        assert!(sidebar_drawer_open(true, false));
        assert!(!sidebar_drawer_open(true, true));
        assert!(!sidebar_drawer_open(false, false));
        assert!(!sidebar_drawer_open(false, true));
    }

    #[test]
    fn layout_class_marks_the_open_drawer() {
        assert_eq!(app_layout_class(false), "app-layout grok-layout");
        assert_eq!(app_layout_class(true), "app-layout grok-layout drawer-open");
    }

    /// Phone headers scroll inside the content; desktop keeps the sticky shell bar.
    #[test]
    fn phone_scroll_header_css_hides_the_sticky_bar_and_keeps_a_menu_fab() {
        let css = include_str!("styles/main.css");
        let block = css
            .split(PHONE_SCROLL_HEADER_CSS_START)
            .nth(1)
            .expect("phone scroll header block");
        let block = block
            .split(PHONE_SCROLL_HEADER_CSS_END)
            .next()
            .expect("phone scroll header end");
        assert!(
            block.contains(".app-header-desktop-root"),
            "desktop shell header must hide on a phone: {block}"
        );
        assert!(
            block.contains(".app-header-in-scroll"),
            "in-flow phone header rules: {block}"
        );
        assert!(
            block.contains("position: static"),
            "phone header must not stay sticky: {block}"
        );
        assert!(
            block.contains(".phone-menu-fab"),
            "floating menu for when the bar scrolls off: {block}"
        );
    }
}
