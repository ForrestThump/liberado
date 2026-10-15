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
use components::incognito::IncognitoToggle;
use components::profile_browser::ProfileChip;
use components::profile_surface::profile_chip_visible;
use components::sidebar::{Route, Sidebar};

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

// ── Grok-style top bar ─────────────────────────────────────────────────────

/// Top-right region: just the incognito pill. The "agent pill" from the first draft was an
/// extraneous waypoint for the user — selecting an agent happens through the Agents shelf in the
/// sidebar, where the rows are already categorised and pinned.
#[component]
fn HeaderRightSide(
    incognito: Signal<bool>,
) -> Element {
    rsx! {
        div {
            class: "header-right-cluster",
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
    let route = use_signal(|| Route::Chat);
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
    let profile_browser_open = use_signal(|| false);
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
                    // The brand wordmark sits at the very left of the top bar. The sidebar has
                    // its own collapse control baked into its header row, so the chrome here is
                    // deliberately minimal — just the brand and the incognito pill.
                    span { class: "brand grok-header-brand", "Liberado" }
                    HeaderRightSide {
                        incognito: incognito,
                    }
                }
            }

            div {
                class: "app-layout grok-layout",
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
                        Route::Imagine => rsx! { crate::components::imagine::Imagine { api_base: base.clone() } },
                        Route::Library => rsx! { crate::components::library::Library { api_base: base.clone() } },
                        Route::Automations => rsx! { crate::components::automations::Automations { api_base: base.clone() } },
                        Route::Projects => rsx! { crate::components::projects::Projects { api_base: base.clone() } },
                        Route::Plugins => rsx! { crate::components::plugins::Plugins { api_base: base.clone() } },
                        Route::Agents => rsx! { crate::components::imagine::Imagine { api_base: base.clone() } },
                    }
                }
            }
        }
    }
}
