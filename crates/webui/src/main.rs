// WIP Dioxus scaffold — many component helpers are wired into planned features but
// not yet called from the render tree. Allow dead code crate-wide until wiring is complete.
#![allow(dead_code)]
use dioxus::prelude::*;

mod components;
mod theme;

use components::chat::Chat;
use components::dashboard::Dashboard;
use components::incognito::IncognitoToggle;
use components::sidebar::Sidebar;

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

fn main() {
    #[cfg(target_arch = "wasm32")]
    dioxus::launch(App);

    #[cfg(not(target_arch = "wasm32"))]
    {
        eprintln!("liberado-webui is a WASM binary — build with:");
        eprintln!("  dx build --release --package liberado-webui --web");
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
    // `mut` because the header's menu button toggles it (see below).
    // Default collapsed on narrow (phone-width) viewports so the sidebar doesn't cover the chat
    // on first load — expanded by default everywhere else, matching prior behavior.
    let mut sidebar_collapsed = use_signal(is_narrow_viewport);

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
                            "\u{2630}"
                        }
                        span { class: "brand", "Liberado" }
                        span { class: "brand-version", "v0.1" }
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
                        }
                    } else {
                        Dashboard { api_base: base.clone() }
                    }
                }
            }
        }
    }
}
