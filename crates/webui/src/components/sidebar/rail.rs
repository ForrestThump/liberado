//! Rail glyphs, primary-nav rows, and the theme swatch.
//!
//! Split from `sidebar.rs` so that file stays inside the module-health boundaries.

use dioxus::prelude::*;

use super::Route;
use crate::components::project_data::close_phone_drawer;

// ── Sub-components ──────────────────────────────────────────────────────────

#[component]
pub(super) fn BrandLogo() -> Element {
    rsx! {
        svg {
            class: "icon sidebar-brand-glyph",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 22 22",
            "aria-hidden": "true",
            circle { cx: "11", cy: "11", r: "10", fill: "var(--lib-accent, #f5a623)" }
            text {
                x: "11",
                y: "15",
                text_anchor: "middle",
                font_family: "system-ui, sans-serif",
                font_size: "12",
                font_weight: "700",
                fill: "var(--lib-app-bg, #0a0a0a)",
                "L"
            }
        }
    }
}

#[component]
pub(super) fn PrimaryNavRow(
    route: Signal<Route>,
    target: Route,
    label: &'static str,
    #[props(into)] icon: Element,
    collapsed: Signal<bool>,
) -> Element {
    let active = match target {
        Route::Chat => route().is_chat_view(),
        _ => route() == target,
    };
    let cls = if active {
        "sidebar-nav-row active"
    } else {
        "sidebar-nav-row"
    };
    rsx! {
        button {
            class: "{cls}",
            r#type: "button",
            title: "{label}",
            "aria-label": "{label}",
            onclick: move |_| {
                route.set(target);
                close_phone_drawer(collapsed);
            },
            {icon}
            span { class: "sidebar-nav-label", "{label}" }
        }
    }
}

// Glyph set — kept inline so the colour swatches and weights match the rest of the rail.

#[component]
pub(super) fn ChatBubble() -> Element {
    rsx! {
        svg {
            class: "icon sidebar-icon",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 22 22",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.6",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            path { d: "M3.5 4.5h13a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H8.5l-3.5 3v-3H3.5a2 2 0 0 1-2-2v-7a2 2 0 0 1 2-2z" }
        }
    }
}

#[component]
pub(super) fn LibraryGlyph() -> Element {
    rsx! {
        svg {
            class: "icon sidebar-icon",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 22 22",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.6",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            rect { x: "5", y: "4", width: "12", height: "13", rx: "1.5" }
            path { d: "M5 8h12" }
            path { d: "M5 12h12" }
        }
    }
}

#[component]
pub(super) fn ClockGlyph() -> Element {
    rsx! {
        svg {
            class: "icon sidebar-icon",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 22 22",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.6",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            circle { cx: "11", cy: "11", r: "7.5" }
            path { d: "M11 6.5V11l3 1.5" }
        }
    }
}

#[component]
pub(super) fn AgentGlyph() -> Element {
    rsx! {
        svg {
            class: "icon sidebar-icon",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 22 22",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.6",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            circle { cx: "11", cy: "8", r: "3.5" }
            path { d: "M4 18a7 7 0 0 1 14 0" }
        }
    }
}

#[component]
pub(super) fn ProjectGlyph() -> Element {
    rsx! {
        svg {
            class: "icon sidebar-icon",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 22 22",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.6",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            path { d: "M3.5 6.5a1 1 0 0 1 1-1H8l2 2h7.5a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1h-13a1 1 0 0 1-1-1z" }
        }
    }
}

#[component]
pub(super) fn PluginGlyph() -> Element {
    rsx! {
        svg {
            class: "icon sidebar-icon",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 22 22",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.6",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            path { d: "M9 4v3" }
            path { d: "M13 4v3" }
            path { d: "M7 7h8v3a4 4 0 0 1-4 4 4 4 0 0 1-4-4z" }
            path { d: "M11 14v4" }
        }
    }
}

/// Chevron pointing left — the user-facing signal that the action will collapse the rail back
/// to the icon-only state.
#[component]
pub(super) fn CollapseGlyph() -> Element {
    rsx! {
        svg {
            class: "icon sidebar-icon",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 22 22",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.6",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            // Arrow that opens to the right when paired with the brand glyph: a single chevron at
            // the very left edge of the brand row, identical to the rotate-by-180 mirror image of
            // the icon in the collapsed-rail state.
            path { d: "M14.5 5l-6 6 6 6" }
        }
    }
}

/// Chevron pointing right — the inverse of `CollapseGlyph`. Sits at the top of the rail when
/// collapsed so the user's eye lands on it first; clicking brings the panel back.
#[component]
pub(super) fn ExpandGlyph() -> Element {
    rsx! {
        svg {
            class: "icon sidebar-icon",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 22 22",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.6",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            // Open-panel glyph: a forward chevron that reads as "push the rail out". Same shape
            // as CollapseGlyph rotated 180°.
            path { d: "M7.5 5l6 6-6 6" }
        }
    }
}

#[component]
fn PaletteGlyph() -> Element {
    rsx! {
        svg {
            class: "icon sidebar-icon",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 22 22",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.6",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            path { d: "M11 3a8 8 0 0 0-1 15.9c.5.1.9-.4.7-.9-.2-.5-.4-1.1-.4-1.6 0-1.1.9-1.4 1.6-1.4H13a3 3 0 0 0 3-3.3 8 8 0 0 0-5-8.7z" }
            circle { cx: "6.5", cy: "11", r: "1" }
            circle { cx: "9", cy: "6.5", r: "1" }
            circle { cx: "14.5", cy: "7.5", r: "1" }
            circle { cx: "17", cy: "11.5", r: "1" }
        }
    }
}

// ── Theme cycling ──────────────────────────────────────────────────────────

/// A compact swatch that lists the registered themes and applies the picked one through the
/// `crate::theme` helpers. Persistence rides on `save_theme_name` — the same backend the `/theme`
/// slash command already uses, so the choice survives across server restarts.
#[component]
pub(super) fn ThemeSwatch(theme_epoch: Signal<u64>) -> Element {
    let mut current = use_signal(crate::theme::saved_theme_name);
    // React to external bumps (mostly a safety net — the picker itself mutates `current` on
    // click and a parent re-render would otherwise not re-sync the dropdown).
    use_effect(move || {
        let _ = theme_epoch();
        current.set(crate::theme::saved_theme_name());
    });
    let names = use_signal(crate::theme::builtin_theme_names);

    let mut open = use_signal(bool::default);

    rsx! {
        div {
            class: "theme-swatch-shell",
            button {
                class: if open() { "theme-swatch-toggle open" } else { "theme-swatch-toggle" },
                r#type: "button",
                title: "Theme",
                "aria-label": "Theme",
                onclick: move |_| open.set(!open()),
                PaletteGlyph {}
            }
            if open() {
                div {
                    class: "theme-swatch-pop",
                    for name in names.read().iter() {
                        {
                            let name_str = name.clone();
                            let active = *name == current();
                            let cls = if active { "theme-swatch-row active" } else { "theme-swatch-row" };
                            let dot = theme_dot(name.as_str());
                            rsx! {
                                button {
                                    class: "{cls}",
                                    r#type: "button",
                                    onclick: move |_| {
                                        current.set(name_str.clone());
                                        crate::theme::save_theme_name(name_str.as_str());
                                        open.set(false);
                                        // Trigger the parent to re-render the CSS block.
                                        theme_epoch += 1;
                                    },
                                    span {
                                        class: "theme-swatch-dot",
                                        style: "background: {dot};",
                                        "aria-hidden": "true"
                                    }
                                    span { class: "theme-swatch-name", "{name}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn theme_dot(name: &str) -> &'static str {
    match name {
        "dark" => "#00ffff",
        "light" => "#c0c0c0",
        "nord" => "#5e81ac",
        "sunset" => "#f5a623",
        "forest" => "#6dba6d",
        "plum" => "#ce93d8",
        "mono" => "#d4a85a",
        _ => "#888888",
    }
}
