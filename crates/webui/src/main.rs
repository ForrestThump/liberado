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
use components::dashboard::Dashboard;
use components::incognito::IncognitoToggle;
use components::profile_browser::ProfileChip;
use components::sidebar::Sidebar;
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
        const NARROW_MAX_PX: f64 = 768.0;
        web_sys::window()
            .and_then(|w| w.inner_width().ok())
            .and_then(|v| v.as_f64())
            .map(|width| width < NARROW_MAX_PX)
            .unwrap_or(false)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        false
    }
}

/// Keep `narrow` in sync with the live viewport.
///
/// `use_signal(is_narrow_viewport)` only samples once. A desktop load then shrinking to 375px
/// used to leave the sidebar expanded, covering the chat, with no layout that could recover.
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
    if let Ok(Some(mql)) = window.match_media("(max-width: 767px)") {
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
}

#[component]
fn App() -> Element {
    let base = api_base();
    let mut view = use_signal(|| "chat");
    // The active theme name, restored from the last choice. Chat writes it when `/theme set` lands.
    let theme_name = use_signal(crate::theme::saved_theme_name);
    let active_conv_id = use_signal(|| None::<String>);
    // Incognito lives up here, not in `Chat`, because it is a mode the whole window is in: the
    // header shows it, the body is tinted for it, and it has to survive a trip through the Status
    // view and back. It always starts **off** — a privacy mode you did not switch on yourself, out
    // of a store you forgot was there, is a mode you cannot reason about.
    let incognito = use_signal(|| false);
    // "New Chat" as an event rather than a state change — see the button in `sidebar.rs`. Owned here
    // because the sidebar raises it and the chat acts on it.
    let new_chat_nonce = use_signal(|| 0u64);
    // The `/model` and `/theme` pickers. They are opened from inside `Chat`, but they live here with
    // every other dismissible layer, because the Back gesture needs one place that knows what is
    // open and in what order (see the block below and `back_nav.rs`).
    let model_browser_open = use_signal(|| false);
    let theme_browser_open = use_signal(|| false);
    // The slash palette. `Chat` reports whether it is showing (openness depends on the input text,
    // which only Chat has) and `App` sets `dismissed` to close it — the same two-signal split any
    // layer whose visibility is derived would need.
    let palette_visible = use_signal(|| false);
    // The session-profile picker, and which profile the open chat runs under. Both live here for
    // the same reason the other pickers do: `App` owns the Back-gesture layer stack.
    let mut profile_browser_open = use_signal(|| false);
    let active_profile = use_signal(|| None::<String>);
    let palette_dismissed = use_signal(|| false);
    // `mut` because the header's menu button toggles it (see below).
    // Default collapsed on narrow (phone-width) viewports so the sidebar doesn't cover the chat
    // on first load — expanded by default everywhere else, matching prior behavior.
    // Tracked as a signal so a resize across the breakpoint is not a permanent layout.
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
    //
    // The sidebar counts only where it is an overlay. On a wide screen it is a persistent panel that
    // starts open, which would mean a guard entry from first paint and a Back press that collapses
    // the conversation list for no reason — the same distinction `collapse_after_pick` draws.
    let sidebar_is_a_layer = move || !sidebar_collapsed() && is_narrow();

    use_hook(|| {
        let mut view = view;
        let mut sidebar_collapsed = sidebar_collapsed;
        let mut model_browser_open = model_browser_open;
        let mut theme_browser_open = theme_browser_open;
        let mut profile_browser_open = profile_browser_open;
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
            } else if !sidebar_collapsed() && is_narrow() {
                sidebar_collapsed.set(true);
            } else if view() != "chat" {
                view.set("chat");
            }
        });
    });

    let back_depth = use_memo(move || {
        [
            palette_visible(),
            model_browser_open(),
            theme_browser_open(),
            profile_browser_open(),
            sidebar_is_a_layer(),
            view() != "chat",
        ]
        .iter()
        .filter(|open| **open)
        .count()
    });
    use_effect(move || back_nav::sync_depth(back_depth()));

    let chat_cls = if view() == "chat" {
        "nav-btn active"
    } else {
        "nav-btn"
    };
    let status_cls = if view() == "status" {
        "nav-btn active"
    } else {
        "nav-btn"
    };

    rsx! {
        style { {crate::theme::theme_css_vars(&crate::theme::theme_by_name(&theme_name()))} }
        style { {include_str!("./styles/main.css")} }

        div {
            class: "app",
            header {
                class: "app-header",
                div {
                    class: "app-header-inner",
                    div {
                        class: "brand-group",
                        // The sidebar's only always-visible control. It lives here rather than in a
                        // collapsed rail so that hiding the conversation list gives the full width
                        // back to the chat.
                        button {
                            class: "menu-btn",
                            onclick: move |_| {
                                let now = sidebar_collapsed();
                                sidebar_collapsed.set(!now);
                            },
                            title: if sidebar_collapsed() { "Show conversations" } else { "Hide conversations" },
                            IconMenu {}
                        }
                        span { class: "brand", "Liberado" }
                        // Profile chip sits here so it does not take a row out of the transcript.
                        ProfileChip {
                            active_profile,
                            on_open: move |_| {
                                // The picker is rendered inside `Chat`. Switching first means a
                                // click from Status still opens it, rather than setting a flag on a
                                // view that is not mounted.
                                view.set("chat");
                                profile_browser_open.set(true);
                            },
                        }
                    }
                    nav {
                        class: "nav",
                        IncognitoToggle { on: incognito }
                        button { class: "{chat_cls}", onclick: move |_| view.set("chat"), "Chat" }
                        button { class: "{status_cls}", onclick: move |_| view.set("status"), "Status" }
                    }
                }
            }
            div {
                class: "app-layout",
                Sidebar {
                    api_base: base.clone(),
                    active_conv_id,
                    collapsed: sidebar_collapsed,
                    new_chat_nonce,
                }
                main {
                    class: "main-content",
                    if view() == "chat" {
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
                        }
                    } else {
                        Dashboard { api_base: base.clone() }
                    }
                }
            }
        }
    }
}
