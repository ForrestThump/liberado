use dioxus::prelude::*;

mod components;

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

    let active = "px-3 py-1 rounded-md text-sm font-medium bg-gray-800 text-indigo-300";
    let inactive = "px-3 py-1 rounded-md text-sm font-medium text-gray-400 hover:text-gray-200";
    let chat_cls = if view() == "chat" { active } else { inactive };
    let status_cls = if view() == "status" { active } else { inactive };

    rsx! {
        div {
            class: "min-h-screen bg-gray-950 text-gray-100 font-sans",
            link { rel: "stylesheet", href: "https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&display=swap" }
            style { {include_str!("./styles/main.css")} }
            header {
                class: "border-b border-gray-800 bg-gray-900/80 backdrop-blur-sm sticky top-0 z-10",
                div {
                    class: "max-w-6xl mx-auto px-4 h-14 flex items-center justify-between",
                    div {
                        class: "flex items-center gap-3",
                        span { class: "text-xl font-bold text-indigo-400", "Liberado" }
                        span { class: "text-sm text-gray-500", "v0.1" }
                    }
                    nav {
                        class: "flex items-center gap-1 bg-gray-900 rounded-lg p-1",
                        button { class: "{chat_cls}", onclick: move |_| view.set("chat"), "Chat" }
                        button { class: "{status_cls}", onclick: move |_| view.set("status"), "Status" }
                    }
                }
            }
            main {
                class: "max-w-6xl mx-auto px-4 py-6",
                if view() == "chat" {
                    Chat { api_base: base.clone() }
                } else {
                    Dashboard { api_base: base.clone() }
                }
            }
        }
    }
}
