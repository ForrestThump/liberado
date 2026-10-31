// WIP Dioxus scaffold — many component helpers are wired into planned features but
// not yet called from the render tree. Allow dead code crate-wide until wiring is complete.
#![allow(dead_code)]
use dioxus::prelude::*;

mod back_nav;
mod components;
mod http;
mod icons;
mod panic_hook;
mod push;
#[cfg(test)]
mod pwa;
mod shell;
mod theme;

use components::chat::Chat;
use components::sidebar::{Route, Sidebar};
use shell::{
    DESKTOP_APP_HEADER_CLASS, GrokAppHeader, PHONE_SCROLL_APP_HEADER_CLASS, PhoneMenuFab,
    shell_route_title,
};

#[cfg(target_arch = "wasm32")]
pub(crate) use shell::watch_viewport_width;
pub(crate) use shell::{app_layout_class, is_narrow_viewport, sidebar_drawer_open};

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

fn main() {
    #[cfg(target_arch = "wasm32")]
    {
        panic_hook::install();
        dioxus::launch(App);
    }

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
}

// ── App ────────────────────────────────────────────────────────────────────

#[component]
fn App() -> Element {
    let base = api_base();
    let mut auth_ready = use_signal(|| false);
    let mut need_login = use_signal(|| false);
    let base_for_session = base.clone();
    use_effect(move || {
        #[cfg(target_arch = "wasm32")]
        {
            let base = base_for_session.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match components::login_gate::probe_session(&base).await {
                    Ok(session) => {
                        need_login.set(session.login_required && !session.authenticated);
                        auth_ready.set(true);
                    }
                    Err(_) => {
                        need_login.set(false);
                        auth_ready.set(true);
                    }
                }
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            auth_ready.set(true);
        }
    });
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
    // Coding project for this window. Chat cannot send it; the projects page says why.
    let active_project = use_signal(|| None::<String>);
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

    let drawer_open = move || sidebar_drawer_open(is_narrow(), sidebar_collapsed());
    use_effect(move || {
        let mut tools_open = tools_open;
        if sidebar_drawer_open(is_narrow(), sidebar_collapsed()) && tools_open() {
            tools_open.set(false);
        }
    });
    let open_profile_picker = move |_| {
        route.set(Route::Chat);
        profile_browser_open.set(true);
    };

    // Top-right cluster is always present — the Grok layout puts a single row up there with the
    // agent pill + private pill, and the open route is reflected through the sidebar's primary nav.

    rsx! {
        style { {crate::theme::theme_css_vars(&crate::theme::theme_by_name(&theme_name()))} }
        style { {include_str!("./styles/main.css")} }

        if !auth_ready() {
            div { class: "login-gate", "Loading session…" }
        } else if need_login() {
            components::login_gate::LoginGate {
                api_base: base.clone(),
                on_authenticated: move |_| need_login.set(false),
            }
        } else {
        div {
            class: "app",
            if !is_narrow() {
                GrokAppHeader {
                    shell_class: DESKTOP_APP_HEADER_CLASS,
                    bar_title: shell_route_title(route()),
                    drawer_open: drawer_open(),
                    sidebar_collapsed,
                    active_project,
                    active_profile,
                    active_surface,
                    on_profile_open: open_profile_picker,
                    incognito,
                    api_base: base.clone(),
                    active_conv_id,
                    tools_open,
                }
            }
            PhoneMenuFab {
                drawer_open: drawer_open(),
                sidebar_collapsed,
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
                    active_project,
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
                                active_project,
                                phone_scroll_header: if is_narrow() {
                                    Some(rsx! {
                                        GrokAppHeader {
                                            shell_class: PHONE_SCROLL_APP_HEADER_CLASS,
                                            bar_title: shell_route_title(route()),
                                            drawer_open: drawer_open(),
                                            sidebar_collapsed,
                                            active_project,
                                            active_profile,
                                            active_surface,
                                            on_profile_open: open_profile_picker,
                                            incognito,
                                            api_base: base.clone(),
                                            active_conv_id,
                                            tools_open,
                                        }
                                    })
                                } else {
                                    None
                                },
                            }
                        },
                        Route::Imagine => rsx! {
                            div { class: "grok-main-phone-scroll",
                                if is_narrow() {
                                    GrokAppHeader {
                                        shell_class: PHONE_SCROLL_APP_HEADER_CLASS,
                                            bar_title: shell_route_title(route()),
                                        drawer_open: drawer_open(),
                                        sidebar_collapsed,
                                        active_project,
                                        active_profile,
                                        active_surface,
                                        on_profile_open: open_profile_picker,
                                        incognito,
                                        api_base: base.clone(),
                                        active_conv_id,
                                        tools_open,
                                    }
                                }
                                crate::components::imagine::Imagine {}
                            }
                        },
                        Route::Library => rsx! {
                            div { class: "grok-main-phone-scroll",
                                if is_narrow() {
                                    GrokAppHeader {
                                        shell_class: PHONE_SCROLL_APP_HEADER_CLASS,
                                            bar_title: shell_route_title(route()),
                                        drawer_open: drawer_open(),
                                        sidebar_collapsed,
                                        active_project,
                                        active_profile,
                                        active_surface,
                                        on_profile_open: open_profile_picker,
                                        incognito,
                                        api_base: base.clone(),
                                        active_conv_id,
                                        tools_open,
                                    }
                                }
                                crate::components::library::Library { api_base: base.clone() }
                            }
                        },
                        Route::Automations => rsx! {
                            div { class: "grok-main-phone-scroll",
                                if is_narrow() {
                                    GrokAppHeader {
                                        shell_class: PHONE_SCROLL_APP_HEADER_CLASS,
                                        bar_title: shell_route_title(route()),
                                        drawer_open: drawer_open(),
                                        sidebar_collapsed,
                                        active_project,
                                        active_profile,
                                        active_surface,
                                        on_profile_open: open_profile_picker,
                                        incognito,
                                        api_base: base.clone(),
                                        active_conv_id,
                                        tools_open,
                                    }
                                }
                                crate::components::automations::Automations {
                                    api_base: base.clone(),
                                    active_conv_id,
                                    active_surface,
                                    list_epoch,
                                    route,
                                    collapsed: sidebar_collapsed,
                                }
                            }
                        },
                        Route::Notifications => rsx! {
                            div { class: "grok-main-phone-scroll",
                                if is_narrow() {
                                    GrokAppHeader {
                                        shell_class: PHONE_SCROLL_APP_HEADER_CLASS,
                                        bar_title: shell_route_title(route()),
                                        drawer_open: drawer_open(),
                                        sidebar_collapsed,
                                        active_project,
                                        active_profile,
                                        active_surface,
                                        on_profile_open: open_profile_picker,
                                        incognito,
                                        api_base: base.clone(),
                                        active_conv_id,
                                        tools_open,
                                    }
                                }
                                crate::components::notifications::Notifications { api_base: base.clone() }
                            }
                        },
                        Route::Projects => rsx! {
                            div { class: "grok-main-phone-scroll",
                                if is_narrow() {
                                    GrokAppHeader {
                                        shell_class: PHONE_SCROLL_APP_HEADER_CLASS,
                                            bar_title: shell_route_title(route()),
                                        drawer_open: drawer_open(),
                                        sidebar_collapsed,
                                        active_project,
                                        active_profile,
                                        active_surface,
                                        on_profile_open: open_profile_picker,
                                        incognito,
                                        api_base: base.clone(),
                                        active_conv_id,
                                        tools_open,
                                    }
                                }
                                crate::components::projects::Projects {
                                    api_base: base.clone(),
                                    active_project,
                                    active_conv_id,
                                    list_epoch,
                                    route,
                                    collapsed: sidebar_collapsed,
                                }
                            }
                        },
                        Route::Plugins => rsx! {
                            div { class: "grok-main-phone-scroll",
                                if is_narrow() {
                                    GrokAppHeader {
                                        shell_class: PHONE_SCROLL_APP_HEADER_CLASS,
                                            bar_title: shell_route_title(route()),
                                        drawer_open: drawer_open(),
                                        sidebar_collapsed,
                                        active_project,
                                        active_profile,
                                        active_surface,
                                        on_profile_open: open_profile_picker,
                                        incognito,
                                        api_base: base.clone(),
                                        active_conv_id,
                                        tools_open,
                                    }
                                }
                                crate::components::plugins::Plugins { api_base: base.clone() }
                            }
                        },
                        Route::Agents => rsx! {
                            div { class: "grok-main-phone-scroll",
                                if is_narrow() {
                                    GrokAppHeader {
                                        shell_class: PHONE_SCROLL_APP_HEADER_CLASS,
                                            bar_title: shell_route_title(route()),
                                        drawer_open: drawer_open(),
                                        sidebar_collapsed,
                                        active_project,
                                        active_profile,
                                        active_surface,
                                        on_profile_open: open_profile_picker,
                                        incognito,
                                        api_base: base.clone(),
                                        active_conv_id,
                                        tools_open,
                                    }
                                }
                                crate::components::imagine::Imagine {}
                            }
                        },
                    }
                }
            }
        }
        }
    }
}
