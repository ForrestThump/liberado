//! Small inline SVG set for chrome. No icon crate — these are a handful of 24×24
//! strokes, colored with `currentColor` so they follow the button they sit in.
//!
//! Two stroke widths in play: the chrome glyphs (menu, plus, search, check, x, chevrons,
//! spinner, stop, copy) stay on the 2px stroke `IconShell` so they match. The composite
//! glyphs (grok shape: chat bubble, imagine image, library stack, automations clock,
//! project folder, plugin plug, mic, voice waveform) sit on a thinner 1.5px stroke that
//! reads light in a sidebar that wants to disappear, not bookend every row.

use dioxus::prelude::*;

#[component]
fn IconShell(children: Element) -> Element {
    rsx! {
        svg {
            class: "icon",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            {children}
        }
    }
}

#[component]
pub fn IconMenu() -> Element {
    rsx! {
        IconShell {
            path { d: "M4 6h16M4 12h16M4 18h16" }
        }
    }
}

#[component]
pub fn IconPlus() -> Element {
    rsx! {
        IconShell {
            line { x1: "12", y1: "5", x2: "12", y2: "19" }
            line { x1: "5", y1: "12", x2: "19", y2: "12" }
        }
    }
}

#[component]
pub fn IconSearch() -> Element {
    rsx! {
        IconShell {
            circle { cx: "11", cy: "11", r: "8" }
            line { x1: "21", y1: "21", x2: "16.65", y2: "16.65" }
        }
    }
}

#[component]
pub fn IconChevronLeft() -> Element {
    rsx! {
        IconShell {
            polyline { points: "15 18 9 12 15 6" }
        }
    }
}

#[component]
pub fn IconChevronRight() -> Element {
    rsx! {
        IconShell {
            polyline { points: "9 18 15 12 9 6" }
        }
    }
}

#[component]
pub fn IconChevronDown() -> Element {
    rsx! {
        IconShell {
            polyline { points: "6 9 12 15 18 9" }
        }
    }
}

#[component]
pub fn IconGlasses() -> Element {
    rsx! {
        IconShell {
            circle { cx: "6.5", cy: "14", r: "3.5" }
            circle { cx: "17.5", cy: "14", r: "3.5" }
            path { d: "M10 14h4" }
            path { d: "M3 14H2M22 14h-1" }
        }
    }
}

#[component]
pub fn IconCheck() -> Element {
    rsx! {
        IconShell {
            polyline { points: "20 6 9 17 4 12" }
        }
    }
}

#[component]
pub fn IconX() -> Element {
    rsx! {
        IconShell {
            line { x1: "18", y1: "6", x2: "6", y2: "18" }
            line { x1: "6", y1: "6", x2: "18", y2: "18" }
        }
    }
}

#[component]
pub fn IconStop() -> Element {
    rsx! {
        svg {
            class: "icon",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 24 24",
            fill: "currentColor",
            stroke: "none",
            "aria-hidden": "true",
            rect {
                x: "6",
                y: "6",
                width: "12",
                height: "12",
                rx: "1.5",
            }
        }
    }
}

#[component]
pub fn IconSpinner() -> Element {
    rsx! {
        IconShell {
            path { d: "M12 3a9 9 0 1 1-9 9" }
        }
    }
}

/// Two stacked rounded rectangles — a sheet above a backing sheet — the standard "copy" glyph.
/// Sized to match the rest of the stroke set (24×24 viewBox, 2px stroke, currentColor).
#[component]
pub fn IconCopy() -> Element {
    rsx! {
        IconShell {
            rect { x: "9", y: "9", width: "11", height: "11", rx: "2" }
            path { d: "M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" }
        }
    }
}

// ── Grok-style sidebar set ──────────────────────────────────────────────────
//
// Sidebar glyphs want a lighter touch: a 1.5px stroke at 22×22 reads as quiet hint, the chrome
// glyphs (2px stroke at 24×24) read as buttons. Mixing the two in the same column would give the
// navigation rows different visual weights than the action glyphs in the chat composer.

#[component]
fn SidebarIcon(children: Element) -> Element {
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
            {children}
        }
    }
}

/// A speech bubble with a short tail — the Chat rail entry. Round path strokes at the corners to
/// match the rest of the sidebar set.
#[component]
pub fn IconChat() -> Element {
    rsx! {
        SidebarIcon {
            path { d: "M3.5 4.5h13a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H8.5l-3.5 3v-3H3.5a2 2 0 0 1-2-2v-7a2 2 0 0 1 2-2z" }
        }
    }
}

/// Mountain inside a rounded square — the Imagine rail entry. A small sun sits behind the peak to
/// mimic Grok's image glyph without copying any branding.
#[component]
pub fn IconImagine() -> Element {
    rsx! {
        SidebarIcon {
            rect { x: "3", y: "4", width: "16", height: "14", rx: "2.5" }
            circle { cx: "8", cy: "9.5", r: "1.4" }
            path { d: "M5 16l4-4.5 3 3 2-2.5 3 4" }
        }
    }
}

/// Three stacked sheets — the Library rail entry. The middle sheet is offset so you can read all
/// three at once.
#[component]
pub fn IconLibrary() -> Element {
    rsx! {
        SidebarIcon {
            rect { x: "5", y: "4", width: "12", height: "13", rx: "1.5" }
            path { d: "M5 8h12" }
            path { d: "M5 12h12" }
        }
    }
}

/// A clock face with a tick mark — Automations rail entry. Hands stay at 12 and 3 so the glyph
/// reads as "scheduled" rather than as a specific time.
#[component]
pub fn IconAutomations() -> Element {
    rsx! {
        SidebarIcon {
            circle { cx: "11", cy: "11", r: "7.5" }
            path { d: "M11 6.5V11l3 1.5" }
        }
    }
}

/// Folder with a tab — Projects rail entry. The tab corner is rounded so it doesn't read as a
/// rectangle beside the folder.
#[component]
pub fn IconProject() -> Element {
    rsx! {
        SidebarIcon {
            path { d: "M3.5 6.5a1 1 0 0 1 1-1H8l2 2h7.5a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1h-13a1 1 0 0 1-1-1z" }
        }
    }
}

/// Two-port plug with a wire — Plugins rail entry. The plug head reads as a hex so it doesn't
/// look like an ordinary outlet.
#[component]
pub fn IconPlugin() -> Element {
    rsx! {
        SidebarIcon {
            path { d: "M9 4v3" }
            path { d: "M13 4v3" }
            path { d: "M7 7h8v3a4 4 0 0 1-4 4 4 4 0 0 1-4-4z" }
            path { d: "M11 14v4" }
        }
    }
}

/// Agent chip — a ring with a smaller ring inside. Read as "user" / "bot" pair at small sizes.
#[component]
pub fn IconAgent() -> Element {
    rsx! {
        SidebarIcon {
            circle { cx: "11", cy: "8", r: "3.5" }
            path { d: "M4 18a7 7 0 0 1 14 0" }
        }
    }
}

/// Curve over horizontal lines — voice waveform glyph.
#[component]
pub fn IconVoice() -> Element {
    rsx! {
        IconShell {
            // waveform bars — short, long, short. 2px stroke reads as a single glyph at 18px.
            path { d: "M3 12h2" }
            path { d: "M7 8v8" }
            path { d: "M11 5v14" }
            path { d: "M15 9v6" }
            path { d: "M19 11v2" }
        }
    }
}

/// Microphone body with a stand — record-audio glyph.
#[component]
pub fn IconMic() -> Element {
    rsx! {
        IconShell {
            rect { x: "9", y: "3", width: "6", height: "11", rx: "3" }
            path { d: "M5 11a7 7 0 0 0 14 0" }
            path { d: "M12 18v3" }
            path { d: "M9 21h6" }
        }
    }
}

/// Paperclip — attach-files glyph.
#[component]
pub fn IconAttach() -> Element {
    rsx! {
        IconShell {
            path { d: "M21 11.5l-9 9a5 5 0 0 1-7-7l9-9a3.5 3.5 0 0 1 5 5l-9 9a2 2 0 0 1-3-3l8-8" }
        }
    }
}

/// Upward arrow into a tray — upload-folder glyph for the home composer.
#[component]
pub fn IconUploadTray() -> Element {
    rsx! {
        IconShell {
            path { d: "M4 16v3a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-3" }
            path { d: "M12 4v12" }
            path { d: "M7 9l5-5 5 5" }
        }
    }
}

/// Home — solid filled corner mark for the brand glyph. Distinguishes the brand row from the
/// nav rows that follow.
#[component]
pub fn IconHomeBrand() -> Element {
    rsx! {
        SidebarIcon {
            path { d: "M11 3.5l-7 6V18a1 1 0 0 0 1 1h3.5v-4.5a1 1 0 0 1 1-1h3a1 1 0 0 1 1 1V19H17a1 1 0 0 0 1-1V9.5z" }
        }
    }
}
