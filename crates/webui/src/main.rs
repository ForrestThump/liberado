// WIP Dioxus scaffold — many component helpers are wired into planned features but
// not yet called from the render tree. Allow dead code crate-wide until wiring is complete.
#![allow(dead_code)]
use dioxus::prelude::*;

mod back_nav;
mod components;
mod icons;
#[cfg(test)]
mod pwa;
mod theme;

use components::chat::Chat;
use components::grants_popover::ToolsPopover;
use components::incognito::IncognitoToggle;
use components::profile_browser::ProfileChip;
use components::profile_surface::profile_chip_visible;
use components::sidebar::{Route, Sidebar};
use icons::IconMenu;

/// Absolute base URL of the daemon's HTTP API.
///
/// **Same origin by default.** Whoever served this page also answers `/api/*`: the daemon does it
/// directly on `:4201`, and the homelab deploy puts an nginx in front that reverse-proxies `/api/`
/// to the daemon on the AI node (see `deploy/homelab/webui/`). Same-origin is what lets the UI live
/// behind Traefik at `https://liberado.homelab.local/` — a hardcoded `:4201` would send the browser
/// to a port Traefik does not listen on — and it costs no CORS preflight.
///
/// The one exception is `dx serve`, which serves the hot-reload build on its own port and cannot
/// proxy. There, and only there, retarget the same host's `:4201`.
///
/// Must stay absolute: `reqwest`'s wasm client parses this through `Url::parse`, which rejects a
/// relative path.
fn api_base() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        const DEV_SERVE_PORT: &str = "8080";
        const DAEMON_PORT: &str = "4201";
        web_sys::window()
            .and_then(|w| {
                let loc = w.location();
                if loc.port().ok()? == DEV_SERVE_PORT {
                    let proto = loc.protocol().ok()?;
                    let host = loc.hostname().ok()?;
                    return Some(format!("{proto}//{host}:{DAEMON_PORT}"));
                }
                // scheme://host[:port] — no trailing slash, so `{base}/api/x` stays well-formed.
                loc.origin().ok()
            })
            .unwrap_or_else(|| "http://127.0.0.1:4201".to_string())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        "http://127.0.0.1:4201".to_string()
    }
}

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
fn watch_viewport_width(mut narrow: Signal<bool>) {
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

fn main() {
    #[cfg(target_arch = "wasm32")]
    dioxus::launch(App);

    #[cfg(not(target_arch = "wasm32"))]
    {
        eprintln!("liberado-webui is a WASM binary — build with:");
        eprintln!("  dx build --release --package liberado-webui --web");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The host build has no browser: these are the stub branches, pinned so the wasm and host
    // halves cannot drift in intent.

    /// The host half of `api_base` is the same loopback the browser falls back to when window
    /// access fails — the daemon's own port on this machine.
    #[test]
    fn host_api_base_is_the_loopback_fallback() {
        assert_eq!(api_base(), "http://127.0.0.1:4201");
    }

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
}

// ── Grok-style top bar ─────────────────────────────────────────────────────

/// Top-right region: the read-only Tools list, then the incognito pill. Agent selection stays
/// on the sidebar shelf. Tools lives here so Back can close it with the other header layers.
#[component]
fn HeaderRightSide(
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

/// Header profile control. `ProfileChip` stays in this file so the PWA test can find it.
/// An Agent surface renders nothing — the chip is a Chats concept.
#[component]
fn HeaderProfileChip(
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

// ── App ────────────────────────────────────────────────────────────────────

#[component]
fn App() -> Element {
    let base = api_base();
    // Default to Chat — the screenshot says Chat is the open surface. Imagine / Library /
    // Automations / Projects / Plugins are reachable via the sidebar.
    let mut route = use_signal(|| Route::Chat);
    // The active theme name, restored from the last choice. Chat writes it when `/theme set` lands,
    // and the sidebar's theme swatch writes it through `save_theme_name`. The `theme_epoch`
    // counter below is the bridge: a write through either path bumps it, and the effect re-reads
    // localStorage into `theme_name` so the CSS variables block at the top of the rsx re-paints.
    let mut theme_name = use_signal(crate::theme::saved_theme_name);
    let theme_epoch = use_signal(u64::default);
    use_effect(move || {
        let _ = theme_epoch();
        theme_name.set(crate::theme::saved_theme_name());
    });
    let active_conv_id = use_signal(|| None::<String>);
    // Incognito lives up here, not in `Chat`, because it is a mode the whole window is in: the
    // header shows it, the body is tinted for it, and it has to survive a trip through the Status
    // view and back.
    let incognito = use_signal(|| false);
    let new_chat_nonce = use_signal(|| 0u64);
    let list_epoch = use_signal(u64::default);
    // Pickers, kept here because Chat owns where they're opened from — they're owned here so the
    // Back-gesture layer stack stays in one place.
    let model_browser_open = use_signal(|| false);
    let theme_browser_open = use_signal(|| false);
    let mut profile_browser_open = use_signal(|| false);
    // Read-only Tools popover. Owned here so Back closes it with the other layers.
    let tools_open = use_signal(|| false);
    let active_profile = use_signal(|| None::<String>);
    let active_surface = use_signal(|| None::<chat_client_contract::SurfaceMode>);
    let palette_visible = use_signal(|| false);
    let palette_dismissed = use_signal(|| false);
    // Sidebar state. Tracks viewport so crossing the breakpoint re-collapses (or re-opens) it.
    let is_narrow = use_signal(is_narrow_viewport);
    #[cfg(target_arch = "wasm32")]
    use_hook(move || watch_viewport_width(is_narrow));
    let mut sidebar_collapsed = use_signal(is_narrow_viewport);
    let mut was_narrow = use_signal(is_narrow_viewport);
    use_effect(move || {
        let now = is_narrow();
        if now != was_narrow() {
            was_narrow.set(now);
            // Crossing the breakpoint adopts the layout that belongs there: overlay closed on
            // a phone, side panel open on a desk. Manual toggle still wins until the next cross.
            sidebar_collapsed.set(now);
        }
    });

    // ── Back gesture ────────────────────────────────────────────────────────────────────────
    //
    // Swipe-back on a phone should close whatever is on top, not leave the app. `back_nav` keeps one
    // history entry per open layer; these two blocks are the only place that says what a "layer" is,
    // so the count and the closer cannot disagree about it.
    let sidebar_is_a_layer = move || !sidebar_collapsed() && is_narrow();

    use_hook(|| {
        let mut route = route;
        let mut sidebar_collapsed = sidebar_collapsed;
        let mut model_browser_open = model_browser_open;
        let mut theme_browser_open = theme_browser_open;
        let mut profile_browser_open = profile_browser_open;
        let mut tools_open = tools_open;
        let mut palette_dismissed = palette_dismissed;
        back_nav::install(move || {
            // Innermost first: the palette hovers over the input, a picker sits on top of the
            // sidebar, and the sidebar sits on top of the view.
            if palette_visible() {
                palette_dismissed.set(true);
            } else if model_browser_open() {
                model_browser_open.set(false);
            } else if theme_browser_open() {
                theme_browser_open.set(false);
            } else if profile_browser_open() {
                profile_browser_open.set(false);
            } else if tools_open() {
                tools_open.set(false);
            } else if !sidebar_collapsed() && is_narrow() {
                sidebar_collapsed.set(true);
            } else if route() != Route::Chat {
                route.set(Route::Chat);
            }
        });
    });

    let back_depth = use_memo(move || {
        [
            palette_visible(),
            model_browser_open(),
            theme_browser_open(),
            profile_browser_open(),
            tools_open(),
            sidebar_is_a_layer(),
            route() != Route::Chat,
        ]
        .iter()
        .filter(|open| **open)
        .count()
    });
    use_effect(move || back_nav::sync_depth(back_depth()));

    // Top-right cluster is always present — the Grok layout puts a single row up there with the
    // agent pill + private pill, and the open route is reflected through the sidebar's primary nav.

    rsx! {
        style { {crate::theme::theme_css_vars(&crate::theme::theme_by_name(&theme_name()))} }
        style { {include_str!("./styles/main.css")} }

        div {
            class: "app",
            header {
                class: "app-header grok-header",
                div {
                    class: "grok-header-inner",
                    // Phone-only. CSS hides it from 769px up, where the icon rail is the control.
                    // Toggles the same collapsed signal the rail uses, so a chat pick still closes it.
                    button {
                        class: "sidebar-menu-btn",
                        r#type: "button",
                        title: if sidebar_drawer_open(is_narrow(), sidebar_collapsed()) { "Close sidebar" } else { "Open sidebar" },
                        "aria-label": if sidebar_drawer_open(is_narrow(), sidebar_collapsed()) { "Close sidebar" } else { "Open sidebar" },
                        "aria-expanded": "{sidebar_drawer_open(is_narrow(), sidebar_collapsed())}",
                        "aria-controls": "sidebar",
                        onclick: move |_| sidebar_collapsed.set(!sidebar_collapsed()),
                        IconMenu {}
                    }
                    span { class: "brand grok-header-brand", "Liberado" }
                    HeaderProfileChip {
                        active_profile,
                        active_surface,
                        on_open: move |_| {
                            // The picker is rendered inside `Chat`. Switching first means a
                            // click from another route still opens it.
                            route.set(Route::Chat);
                            profile_browser_open.set(true);
                        },
                    }
                    HeaderRightSide {
                        incognito,
                        api_base: base.clone(),
                        active_conv_id,
                        tools_open,
                    }
                }
            }

            div {
                class: "{app_layout_class(sidebar_drawer_open(is_narrow(), sidebar_collapsed()))}",
                if sidebar_drawer_open(is_narrow(), sidebar_collapsed()) {
                    button {
                        class: "sidebar-backdrop",
                        r#type: "button",
                        "aria-label": "Close sidebar",
                        onclick: move |_| sidebar_collapsed.set(true),
                    }
                }
                Sidebar {
                    api_base: base.clone(),
                    route: route,
                    active_conv_id,
                    collapsed: sidebar_collapsed,
                    new_chat_nonce,
                    active_surface,
                    list_epoch,
                    theme_epoch,
                }
                main {
                    class: "main-content grok-main",
                    match route() {
                        Route::Chat | Route::ChatSession | Route::AgentSession | Route::ProjectDetail => rsx! {
                            Chat {
                                api_base: base.clone(),
                                active_conv_id,
                                theme_name,
                                incognito,
                                new_chat_nonce,
                                model_browser_open,
                                theme_browser_open,
                                palette_dismissed,
                                palette_visible,
                                profile_browser_open,
                                active_profile,
                                active_surface,
                                list_epoch,
                            }
                        },
                        Route::Imagine => rsx! { crate::components::imagine::Imagine {} },
                        Route::Library => rsx! { crate::components::library::Library {} },
                        Route::Automations => rsx! { crate::components::automations::Automations {} },
                        Route::Projects => rsx! { crate::components::projects::Projects {} },
                        Route::Plugins => rsx! { crate::components::plugins::Plugins { api_base: base.clone() } },
                        Route::Agents => rsx! { crate::components::imagine::Imagine {} },
                    }
                }
            }
        }
    }
}
