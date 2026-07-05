use dioxus::prelude::*;

mod components;
mod theme;

use components::chat::Chat;
use components::dashboard::Dashboard;
use components::sidebar::Sidebar;

fn api_base() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        const API_PORT: &str = "4201";
        web_sys::window()
            .and_then(|w| {
                let loc = w.location();
                let proto = loc.protocol().ok()?;
                let host = loc.hostname().ok()?;
                Some(format!("{proto}//{host}:{API_PORT}"))
            })
            .unwrap_or_else(|| "http://127.0.0.1:4201".to_string())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        "http://127.0.0.1:4201".to_string()
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
    let active_conv_id = use_signal(|| None::<String>);
    // Default collapsed on narrow (phone-width) viewports so the sidebar doesn't cover the chat
    // on first load — expanded by default everywhere else, matching prior behavior.
    let sidebar_collapsed = use_signal(|| {
        #[cfg(target_arch = "wasm32")]
        {
            web_sys::window()
                .and_then(|w| w.inner_width().ok())
                .and_then(|v| v.as_f64())
                .map(|width| width < 768.0)
                .unwrap_or(false)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            false
        }
    });

    let chat_cls = if view() == "chat" { "nav-btn active" } else { "nav-btn" };
    let status_cls = if view() == "status" { "nav-btn active" } else { "nav-btn" };

    rsx! {
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
            div {
                class: "app-layout",
                Sidebar {
                    api_base: base.clone(),
                    active_conv_id,
                    collapsed: sidebar_collapsed,
                }
                main {
                    class: "main-content",
                    if view() == "chat" {
                        Chat {
                            api_base: base.clone(),
                            active_conv_id,
                        }
                    } else {
                        Dashboard { api_base: base.clone() }
                    }
                }
            }
        }
    }
}
