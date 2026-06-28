use dioxus::prelude::*;

mod components;
mod theme;

use components::chat::Chat;
use components::dashboard::Dashboard;

/// Derive the API base from wherever the page was loaded from so LAN access works
/// without any hardcoded IP.  Falls back to localhost only as a dev default.
#[cfg(target_arch = "wasm32")]
fn api_base() -> String {
    // The daemon's API always listens on port 4201. When the daemon also serves this
    // page, that's the same origin; when `dx serve` serves it on another port (dev
    // hot-reload), we still target the daemon's port on the same host — the daemon's
    // permissive CORS allows the cross-port call. This keeps both serving models working.
    const API_PORT: &str = "4201";
    web_sys::window()
        .and_then(|w| {
            let loc = w.location();
            let proto = loc.protocol().ok()?; // "http:" or "https:"
            let host = loc.hostname().ok()?; // hostname, without any port
            Some(format!("{proto}//{host}:{API_PORT}"))
        })
        .unwrap_or_else(|| "http://127.0.0.1:4201".to_string())
}

#[cfg(not(target_arch = "wasm32"))]
fn api_base() -> String {
    "http://127.0.0.1:4201".to_string()
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

    let chat_cls = if view() == "chat" { "nav-btn active" } else { "nav-btn" };
    let status_cls = if view() == "status" { "nav-btn active" } else { "nav-btn" };

    rsx! {
        // Inject theme CSS variables before the main stylesheet so every selector
        // can reference var(--lib-*).  Swapping the Theme arg here is all it takes
        // to switch themes at runtime.
        style { {crate::theme::theme_css_vars(&liberado_theme::Theme::default_dark())} }
        style { {include_str!("./styles/main.css")} }

        div {
            class: "app",
            header {
                class: "app-header",
                div {
                    class: "app-header-inner",
                    div {
                        class: "brand-group",
                        span { class: "brand", "Liberado" }
                        span { class: "brand-version", "v0.1" }
                    }
                    nav {
                        class: "nav",
                        button { class: "{chat_cls}", onclick: move |_| view.set("chat"), "Chat" }
                        button { class: "{status_cls}", onclick: move |_| view.set("status"), "Status" }
                    }
                }
            }
            main {
                class: "main",
                if view() == "chat" {
                    Chat { api_base: base.clone() }
                } else {
                    Dashboard { api_base: base.clone() }
                }
            }
        }
    }
}
