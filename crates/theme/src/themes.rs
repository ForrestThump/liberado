//! Built-in theme palettes.
//!
//! Split from the crate root so `lib.rs` stays inside the module-health
//! function boundary. `Theme::builtins` is the only registration list; the
//! registry copies it on create and on reload.

use super::Theme;

impl Theme {
    /// The full set of built-in themes, in the order the registry exposes them by default.
    /// `ThemeRegistry::new` and `ThemeRegistry::reload` both use this list, so adding a new
    /// theme here is the only place to register a built-in upstream.
    pub fn builtins() -> [Theme; 7] {
        [
            Theme::default_dark(),
            Theme::default_light(),
            Theme::default_nord(),
            Theme::default_sunset(),
            Theme::default_forest(),
            Theme::default_plum(),
            Theme::default_mono(),
        ]
    }
    /// Return the built-in default theme (dark terminal palette).
    pub fn default_dark() -> Self {
        Self {
            name: "dark".into(),
            chat_user_prefix: Some("#00ffff".into()),
            chat_user_text: Some("#ffffff".into()),
            // Slightly lifted from app_bg — readable strip without high contrast.
            chat_user_bg: Some("#16162a".into()),
            chat_assistant_text: Some("#c0c0c0".into()),
            chat_system_text: Some("#808080".into()),
            chat_streaming_cursor: Some("#00ffff".into()),
            tool_label: Some("#ffff00".into()),
            tool_name: Some("#ffff00".into()),
            tool_args: Some("#808080".into()),
            tool_ok: Some("#00ff00".into()),
            tool_err: Some("#ff0000".into()),
            code_block_header: Some("#808000".into()),
            code_block_bg: Some("#303030".into()),
            code_block_fg: Some("#c0c0c0".into()),
            input_border_focused: Some("#00ffff".into()),
            input_border_unfocused: Some("#404040".into()),
            input_placeholder: Some("#404040".into()),
            input_text: Some("#ffffff".into()),
            input_bg: Some("#1a1a2e".into()),
            status_bar_text: Some("#808080".into()),
            status_dot_online: Some("#00ff00".into()),
            status_dot_offline: Some("#ff0000".into()),
            status_dot_connecting: Some("#ffff00".into()),
            reaction_observed: Some("#00ffff".into()),
            reaction_dispatched: Some("#ffff00".into()),
            reaction_acted: Some("#00ff00".into()),
            reaction_unknown: Some("#808080".into()),
            sidebar_selected_bg: Some("#00ffff".into()),
            sidebar_selected_fg: Some("#000000".into()),
            sidebar_text: Some("#c0c0c0".into()),
            sidebar_border_focused: Some("#00ffff".into()),
            sidebar_border_unfocused: Some("#808080".into()),
            sidebar_item_bg: Some("#101010".into()),
            md_bold: Some("#ffffff".into()),
            md_italic: Some("#c0c0c0".into()),
            md_code: Some("#ffff00".into()),
            md_link: Some("#8080ff".into()),
            md_bullet: Some("#00ffff".into()),
            md_heading: Some("#ffffff".into()),
            md_rule: Some("#404040".into()),
            accent: Some("#00ffff".into()),
            border: Some("#808080".into()),
            app_bg: Some("#0d0d1a".into()),
        }
    }

    /// Return the built-in light theme.
    pub fn default_light() -> Self {
        Self {
            name: "light".into(),
            chat_user_prefix: Some("#008080".into()),
            chat_user_text: Some("#1a1a1a".into()),
            // Soft cool wash on white — separates user turns without glare.
            chat_user_bg: Some("#eef6f6".into()),
            chat_assistant_text: Some("#404040".into()),
            chat_system_text: Some("#808080".into()),
            chat_streaming_cursor: Some("#008080".into()),
            tool_label: Some("#808000".into()),
            tool_name: Some("#808000".into()),
            tool_args: Some("#808080".into()),
            tool_ok: Some("#008000".into()),
            tool_err: Some("#cc0000".into()),
            code_block_header: Some("#606000".into()),
            code_block_bg: Some("#f0f0f0".into()),
            code_block_fg: Some("#404040".into()),
            input_border_focused: Some("#008080".into()),
            input_border_unfocused: Some("#c0c0c0".into()),
            input_placeholder: Some("#c0c0c0".into()),
            input_text: Some("#1a1a1a".into()),
            input_bg: Some("#f5f5f5".into()),
            status_bar_text: Some("#808080".into()),
            status_dot_online: Some("#008000".into()),
            status_dot_offline: Some("#cc0000".into()),
            status_dot_connecting: Some("#808000".into()),
            reaction_observed: Some("#008080".into()),
            reaction_dispatched: Some("#808000".into()),
            reaction_acted: Some("#008000".into()),
            reaction_unknown: Some("#808080".into()),
            sidebar_selected_bg: Some("#008080".into()),
            sidebar_selected_fg: Some("#ffffff".into()),
            sidebar_text: Some("#404040".into()),
            sidebar_border_focused: Some("#008080".into()),
            sidebar_border_unfocused: Some("#808080".into()),
            sidebar_item_bg: Some("#f0f0f0".into()),
            md_bold: Some("#1a1a1a".into()),
            md_italic: Some("#404040".into()),
            md_code: Some("#808000".into()),
            md_link: Some("#0066cc".into()),
            md_bullet: Some("#008080".into()),
            md_heading: Some("#1a1a1a".into()),
            md_rule: Some("#c0c0c0".into()),
            accent: Some("#008080".into()),
            border: Some("#c0c0c0".into()),
            app_bg: Some("#ffffff".into()),
        }
    }

    /// Built-in [Nord](https://www.nordtheme.com/) polar-night theme.
    ///
    /// Colors from the official palettes
    /// ([docs](https://www.nordtheme.com/docs/colors-and-palettes)):
    /// polar night `nord0`–`nord3`, snow storm `nord4`–`nord6`, frost `nord7`–`nord10`,
    /// aurora `nord11`–`nord15`.
    pub fn default_nord() -> Self {
        // Polar Night
        const N0: &str = "#2e3440";
        const N1: &str = "#3b4252";
        const N2: &str = "#434c5e";
        const N3: &str = "#4c566a";
        // Snow Storm
        const N4: &str = "#d8dee9";
        const N5: &str = "#e5e9f0";
        const N6: &str = "#eceff4";
        // Frost
        const N7: &str = "#8fbcbb";
        const N8: &str = "#88c0d0";
        const N9: &str = "#81a1c1";
        const N10: &str = "#5e81ac";
        // Aurora
        const N11: &str = "#bf616a";
        const N12: &str = "#d08770";
        const N13: &str = "#ebcb8b";
        const N14: &str = "#a3be8c";
        const N15: &str = "#b48ead";

        Self {
            name: "nord".into(),
            chat_user_prefix: Some(N8.into()),
            chat_user_text: Some(N6.into()),
            // nord1 elevated panel — one step up from polar-night app_bg (nord0).
            chat_user_bg: Some(N1.into()),
            chat_assistant_text: Some(N4.into()),
            chat_system_text: Some(N3.into()),
            chat_streaming_cursor: Some(N8.into()),
            tool_label: Some(N13.into()),
            tool_name: Some(N12.into()),
            tool_args: Some(N3.into()),
            tool_ok: Some(N14.into()),
            tool_err: Some(N11.into()),
            code_block_header: Some(N10.into()),
            code_block_bg: Some(N1.into()),
            code_block_fg: Some(N4.into()),
            input_border_focused: Some(N8.into()),
            input_border_unfocused: Some(N3.into()),
            input_placeholder: Some(N3.into()),
            input_text: Some(N6.into()),
            input_bg: Some(N1.into()),
            status_bar_text: Some(N3.into()),
            status_dot_online: Some(N14.into()),
            status_dot_offline: Some(N11.into()),
            status_dot_connecting: Some(N13.into()),
            reaction_observed: Some(N8.into()),
            reaction_dispatched: Some(N13.into()),
            reaction_acted: Some(N14.into()),
            reaction_unknown: Some(N15.into()),
            sidebar_selected_bg: Some(N2.into()),
            sidebar_selected_fg: Some(N6.into()),
            sidebar_text: Some(N4.into()),
            sidebar_border_focused: Some(N8.into()),
            sidebar_border_unfocused: Some(N3.into()),
            sidebar_item_bg: Some(N0.into()),
            md_bold: Some(N6.into()),
            md_italic: Some(N5.into()),
            md_code: Some(N13.into()),
            md_link: Some(N9.into()),
            md_bullet: Some(N8.into()),
            md_heading: Some(N7.into()),
            md_rule: Some(N3.into()),
            accent: Some(N8.into()),
            border: Some(N3.into()),
            app_bg: Some(N0.into()),
        }
    }

    /// Built-in `sunset` theme — warm amber / coral complementary palette on a deep brick-black
    /// background. The accent stays warm so the prompt and the selected-state row share a single
    /// hue, and assistant foreground is shifted slightly off-white to read against the same warm
    /// ground without bleaching the markdown headings.
    pub fn default_sunset() -> Self {
        Self {
            name: "sunset".into(),
            chat_user_prefix: Some("#f5a623".into()),
            chat_user_text: Some("#f7e8d0".into()),
            chat_user_bg: Some("#231510".into()),
            chat_assistant_text: Some("#e8e0d8".into()),
            chat_system_text: Some("#9a8a78".into()),
            chat_streaming_cursor: Some("#f5a623".into()),
            tool_label: Some("#f5a623".into()),
            tool_name: Some("#ffb366".into()),
            tool_args: Some("#9a8a78".into()),
            tool_ok: Some("#6dba6d".into()),
            tool_err: Some("#e85a5a".into()),
            code_block_header: Some("#a3761f".into()),
            code_block_bg: Some("#1f1410".into()),
            code_block_fg: Some("#e8d8c4".into()),
            input_border_focused: Some("#f5a623".into()),
            input_border_unfocused: Some("#3a2a1a".into()),
            input_placeholder: Some("#5a4434".into()),
            input_text: Some("#f7e8d0".into()),
            input_bg: Some("#1a120e".into()),
            status_bar_text: Some("#9a8a78".into()),
            status_dot_online: Some("#6dba6d".into()),
            status_dot_offline: Some("#e85a5a".into()),
            status_dot_connecting: Some("#f5a623".into()),
            reaction_observed: Some("#ffb366".into()),
            reaction_dispatched: Some("#f5a623".into()),
            reaction_acted: Some("#6dba6d".into()),
            reaction_unknown: Some("#5a4434".into()),
            sidebar_selected_bg: Some("#f5a623".into()),
            sidebar_selected_fg: Some("#1a1010".into()),
            sidebar_text: Some("#d8c8b8".into()),
            sidebar_border_focused: Some("#f5a623".into()),
            sidebar_border_unfocused: Some("#3a2a1a".into()),
            sidebar_item_bg: Some("#16100c".into()),
            md_bold: Some("#f7e8d0".into()),
            md_italic: Some("#d8c8b8".into()),
            md_code: Some("#ffb366".into()),
            md_link: Some("#ffb366".into()),
            md_bullet: Some("#f5a623".into()),
            md_heading: Some("#ffd9a8".into()),
            md_rule: Some("#3a2a1a".into()),
            accent: Some("#f5a623".into()),
            border: Some("#3a2a1a".into()),
            app_bg: Some("#1a1010".into()),
        }
    }

    /// Built-in `forest` theme — sage / amber complementary palette on a deep moss background.
    /// Cool greens for tool and status "ok" channels, warm amber for the prompt accent — never
    /// both at the same strength, so the eye lands on the prompt first.
    pub fn default_forest() -> Self {
        Self {
            name: "forest".into(),
            chat_user_prefix: Some("#6dba6d".into()),
            chat_user_text: Some("#c8e6c9".into()),
            chat_user_bg: Some("#0f1a14".into()),
            chat_assistant_text: Some("#c0d6c4".into()),
            chat_system_text: Some("#708070".into()),
            chat_streaming_cursor: Some("#6dba6d".into()),
            tool_label: Some("#d4a85a".into()),
            tool_name: Some("#d4a85a".into()),
            tool_args: Some("#708070".into()),
            tool_ok: Some("#6dba6d".into()),
            tool_err: Some("#cf6679".into()),
            code_block_header: Some("#4a7a4a".into()),
            code_block_bg: Some("#0e1810".into()),
            code_block_fg: Some("#c8e6c9".into()),
            input_border_focused: Some("#6dba6d".into()),
            input_border_unfocused: Some("#2a3a2a".into()),
            input_placeholder: Some("#3a4a3a".into()),
            input_text: Some("#c8e6c9".into()),
            input_bg: Some("#0e1810".into()),
            status_bar_text: Some("#708070".into()),
            status_dot_online: Some("#6dba6d".into()),
            status_dot_offline: Some("#cf6679".into()),
            status_dot_connecting: Some("#d4a85a".into()),
            reaction_observed: Some("#6dba6d".into()),
            reaction_dispatched: Some("#d4a85a".into()),
            reaction_acted: Some("#6dba6d".into()),
            reaction_unknown: Some("#3a4a3a".into()),
            sidebar_selected_bg: Some("#6dba6d".into()),
            sidebar_selected_fg: Some("#0f1a14".into()),
            sidebar_text: Some("#a8c4a8".into()),
            sidebar_border_focused: Some("#6dba6d".into()),
            sidebar_border_unfocused: Some("#2a3a2a".into()),
            sidebar_item_bg: Some("#0c140c".into()),
            md_bold: Some("#c8e6c9".into()),
            md_italic: Some("#a8c4a8".into()),
            md_code: Some("#d4a85a".into()),
            md_link: Some("#80cbc4".into()),
            md_bullet: Some("#6dba6d".into()),
            md_heading: Some("#a5d6a7".into()),
            md_rule: Some("#2a3a2a".into()),
            accent: Some("#6dba6d".into()),
            border: Some("#2a3a2a".into()),
            app_bg: Some("#0f1a14".into()),
        }
    }

    /// Built-in `plum` theme — lavender / berry complementary palette on a deep aubergine
    /// background. The accent pulls cool, the headings pull warm, both stay inside the magenta
    /// family so the chat reads as one place rather than two washes.
    pub fn default_plum() -> Self {
        Self {
            name: "plum".into(),
            chat_user_prefix: Some("#ce93d8".into()),
            chat_user_text: Some("#e1bee7".into()),
            chat_user_bg: Some("#1a1320".into()),
            chat_assistant_text: Some("#d0c8d8".into()),
            chat_system_text: Some("#8a7a92".into()),
            chat_streaming_cursor: Some("#ce93d8".into()),
            tool_label: Some("#e1bee7".into()),
            tool_name: Some("#ce93d8".into()),
            tool_args: Some("#8a7a92".into()),
            tool_ok: Some("#81c784".into()),
            tool_err: Some("#ef9a9a".into()),
            code_block_header: Some("#7a5a8a".into()),
            code_block_bg: Some("#161018".into()),
            code_block_fg: Some("#e1bee7".into()),
            input_border_focused: Some("#ce93d8".into()),
            input_border_unfocused: Some("#3a2a40".into()),
            input_placeholder: Some("#5a4a60".into()),
            input_text: Some("#e1bee7".into()),
            input_bg: Some("#18101e".into()),
            status_bar_text: Some("#8a7a92".into()),
            status_dot_online: Some("#81c784".into()),
            status_dot_offline: Some("#ef9a9a".into()),
            status_dot_connecting: Some("#e1bee7".into()),
            reaction_observed: Some("#ce93d8".into()),
            reaction_dispatched: Some("#e1bee7".into()),
            reaction_acted: Some("#81c784".into()),
            reaction_unknown: Some("#5a4a60".into()),
            sidebar_selected_bg: Some("#ce93d8".into()),
            sidebar_selected_fg: Some("#18101e".into()),
            sidebar_text: Some("#c8b8d0".into()),
            sidebar_border_focused: Some("#ce93d8".into()),
            sidebar_border_unfocused: Some("#3a2a40".into()),
            sidebar_item_bg: Some("#140c1a".into()),
            md_bold: Some("#e1bee7".into()),
            md_italic: Some("#c8b8d0".into()),
            md_code: Some("#b39ddb".into()),
            md_link: Some("#b39ddb".into()),
            md_bullet: Some("#ce93d8".into()),
            md_heading: Some("#e1bee7".into()),
            md_rule: Some("#3a2a40".into()),
            accent: Some("#ce93d8".into()),
            border: Some("#3a2a40".into()),
            app_bg: Some("#14101a".into()),
        }
    }

    /// Built-in `mono` theme — neutral grayscale with a single warm-amber accent. The selection
    /// row stays amber so the active chat still pops; everything else drops to grey so a long
    /// transcript reads like print, not neon.
    pub fn default_mono() -> Self {
        Self {
            name: "mono".into(),
            chat_user_prefix: Some("#d4a85a".into()),
            chat_user_text: Some("#d0d0d0".into()),
            chat_user_bg: Some("#161616".into()),
            chat_assistant_text: Some("#b0b0b0".into()),
            chat_system_text: Some("#707070".into()),
            chat_streaming_cursor: Some("#d4a85a".into()),
            tool_label: Some("#d4a85a".into()),
            tool_name: Some("#d0d0d0".into()),
            tool_args: Some("#707070".into()),
            tool_ok: Some("#b0b0b0".into()),
            tool_err: Some("#cc6666".into()),
            code_block_header: Some("#909090".into()),
            code_block_bg: Some("#141414".into()),
            code_block_fg: Some("#d0d0d0".into()),
            input_border_focused: Some("#d4a85a".into()),
            input_border_unfocused: Some("#303030".into()),
            input_placeholder: Some("#5a5a5a".into()),
            input_text: Some("#d0d0d0".into()),
            input_bg: Some("#161616".into()),
            status_bar_text: Some("#707070".into()),
            status_dot_online: Some("#b0b0b0".into()),
            status_dot_offline: Some("#cc6666".into()),
            status_dot_connecting: Some("#d4a85a".into()),
            reaction_observed: Some("#d4a85a".into()),
            reaction_dispatched: Some("#d0d0d0".into()),
            reaction_acted: Some("#b0b0b0".into()),
            reaction_unknown: Some("#5a5a5a".into()),
            sidebar_selected_bg: Some("#d4a85a".into()),
            sidebar_selected_fg: Some("#161616".into()),
            sidebar_text: Some("#b0b0b0".into()),
            sidebar_border_focused: Some("#d4a85a".into()),
            sidebar_border_unfocused: Some("#303030".into()),
            sidebar_item_bg: Some("#0e0e0e".into()),
            md_bold: Some("#e0e0e0".into()),
            md_italic: Some("#b0b0b0".into()),
            md_code: Some("#d4a85a".into()),
            md_link: Some("#909090".into()),
            md_bullet: Some("#b0b0b0".into()),
            md_heading: Some("#e0e0e0".into()),
            md_rule: Some("#303030".into()),
            accent: Some("#d4a85a".into()),
            border: Some("#303030".into()),
            app_bg: Some("#0e0e0e".into()),
        }
    }
}
