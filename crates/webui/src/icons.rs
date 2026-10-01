//! Small inline SVG set for chrome. No icon crate — these are a handful of 24×24
//! strokes, colored with `currentColor` so they follow the button they sit in.

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
