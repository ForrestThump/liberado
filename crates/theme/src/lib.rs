//! # liberado-theme
//!
//! Shared theme definitions for all Liberado user interfaces.
//!
//! A [`Theme`] is a flat set of color tokens (no nesting, no UI coupling). Each token
//! maps an abstract role (e.g. `chat_assistant_text`) to an RGB hex string. TOML files
//! are the canonical serialization; every UI loads the same files and maps tokens to its
//! own rendering primitives.
//!
//! [`ThemeRegistry`] discovers themes from `<config>/liberado/themes/*.toml` and merges
//! them with built-in defaults. User themes can override built-in colors per-token.
//!
//! ## Consumers
//!
//! | UI       | Mapping from `Theme` |
//! |----------|----------------------|
//! | TUI      | `ratatui::style::Color::Rgb(r,g,b)` from `parse_hex()` |
//! | Web UI   | CSS custom properties (`--liberado-chat-assistant-text: #...`) |
//! | CLI/daemon | (future) terminal escape codes |

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A flat set of semantic color tokens. Consumers map these to their own rendering
/// primitives — ratatui `Color` for the TUI, CSS variables for the web UI, escape codes
/// for terminal output.
///
/// Every field is an RGB hex string (e.g. `"#c0c0c0"`). `Option` means "use the
/// consumer's default for that token"; a present key always overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Theme {
    #[serde(default)]
    pub name: String,

    // ── Chat pane ──────────────────────────────────────────
    #[serde(default)]
    pub chat_user_prefix: Option<String>,
    #[serde(default)]
    pub chat_user_text: Option<String>,
    #[serde(default)]
    pub chat_assistant_text: Option<String>,
    #[serde(default)]
    pub chat_system_text: Option<String>,
    #[serde(default)]
    pub chat_streaming_cursor: Option<String>,

    // ── Tool chips ─────────────────────────────────────────
    #[serde(default)]
    pub tool_label: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_args: Option<String>,
    #[serde(default)]
    pub tool_ok: Option<String>,
    #[serde(default)]
    pub tool_err: Option<String>,

    // ── Code blocks ────────────────────────────────────────
    #[serde(default)]
    pub code_block_header: Option<String>,
    #[serde(default)]
    pub code_block_bg: Option<String>,
    #[serde(default)]
    pub code_block_fg: Option<String>,

    // ── Input line ─────────────────────────────────────────
    #[serde(default)]
    pub input_border_focused: Option<String>,
    #[serde(default)]
    pub input_border_unfocused: Option<String>,
    #[serde(default)]
    pub input_placeholder: Option<String>,
    #[serde(default)]
    pub input_text: Option<String>,

    // ── Status bar ─────────────────────────────────────────
    #[serde(default)]
    pub status_bar_text: Option<String>,
    #[serde(default)]
    pub status_dot_online: Option<String>,
    #[serde(default)]
    pub status_dot_offline: Option<String>,
    #[serde(default)]
    pub status_dot_connecting: Option<String>,

    // ── Reactions ──────────────────────────────────────────
    #[serde(default)]
    pub reaction_observed: Option<String>,
    #[serde(default)]
    pub reaction_dispatched: Option<String>,
    #[serde(default)]
    pub reaction_acted: Option<String>,
    #[serde(default)]
    pub reaction_unknown: Option<String>,

    // ── Sidebar ────────────────────────────────────────────
    #[serde(default)]
    pub sidebar_selected_bg: Option<String>,
    #[serde(default)]
    pub sidebar_selected_fg: Option<String>,
    #[serde(default)]
    pub sidebar_text: Option<String>,
    #[serde(default)]
    pub sidebar_border_focused: Option<String>,
    #[serde(default)]
    pub sidebar_border_unfocused: Option<String>,
    #[serde(default)]
    pub sidebar_item_bg: Option<String>,

    // ── Markdown ───────────────────────────────────────────
    #[serde(default)]
    pub md_bold: Option<String>,
    #[serde(default)]
    pub md_italic: Option<String>,
    #[serde(default)]
    pub md_code: Option<String>,
    #[serde(default)]
    pub md_link: Option<String>,
    #[serde(default)]
    pub md_bullet: Option<String>,
    #[serde(default)]
    pub md_heading: Option<String>,
    #[serde(default)]
    pub md_rule: Option<String>,

    // ── General ────────────────────────────────────────────
    #[serde(default)]
    pub accent: Option<String>,
    pub border: Option<String>,
}

impl Theme {
    /// Return the built-in default theme (dark terminal palette).
    pub fn default_dark() -> Self {
        Self {
            name: "dark".into(),
            chat_user_prefix: Some("#00ffff".into()),
            chat_user_text: Some("#ffffff".into()),
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
        }
    }

    /// Return the built-in light theme.
    pub fn default_light() -> Self {
        Self {
            name: "light".into(),
            chat_user_prefix: Some("#008080".into()),
            chat_user_text: Some("#1a1a1a".into()),
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
        }
    }

    /// Resolve a color token: return the override if `Some`, otherwise the `fallback` hex
    /// string.
    pub fn resolve(&self, value: &Option<String>, fallback: &str) -> String {
        value.clone().unwrap_or_else(|| fallback.to_string())
    }

    /// Fill in any `None` tokens from `base`. The returned theme has every token from
    /// `self`, with missing tokens inherited from `base`. Name comes from `self`.
    pub fn layered_on(&self, base: &Theme) -> Theme {
        let inherit_string = |a: &Option<String>, b: &Option<String>| {
            a.clone().or_else(|| b.clone())
        };
        Theme {
            name: self.name.clone(),
            chat_user_prefix: inherit_string(&self.chat_user_prefix, &base.chat_user_prefix),
            chat_user_text: inherit_string(&self.chat_user_text, &base.chat_user_text),
            chat_assistant_text: inherit_string(&self.chat_assistant_text, &base.chat_assistant_text),
            chat_system_text: inherit_string(&self.chat_system_text, &base.chat_system_text),
            chat_streaming_cursor: inherit_string(&self.chat_streaming_cursor, &base.chat_streaming_cursor),
            tool_label: inherit_string(&self.tool_label, &base.tool_label),
            tool_name: inherit_string(&self.tool_name, &base.tool_name),
            tool_args: inherit_string(&self.tool_args, &base.tool_args),
            tool_ok: inherit_string(&self.tool_ok, &base.tool_ok),
            tool_err: inherit_string(&self.tool_err, &base.tool_err),
            code_block_header: inherit_string(&self.code_block_header, &base.code_block_header),
            code_block_bg: inherit_string(&self.code_block_bg, &base.code_block_bg),
            code_block_fg: inherit_string(&self.code_block_fg, &base.code_block_fg),
            input_border_focused: inherit_string(&self.input_border_focused, &base.input_border_focused),
            input_border_unfocused: inherit_string(&self.input_border_unfocused, &base.input_border_unfocused),
            input_placeholder: inherit_string(&self.input_placeholder, &base.input_placeholder),
            input_text: inherit_string(&self.input_text, &base.input_text),
            status_bar_text: inherit_string(&self.status_bar_text, &base.status_bar_text),
            status_dot_online: inherit_string(&self.status_dot_online, &base.status_dot_online),
            status_dot_offline: inherit_string(&self.status_dot_offline, &base.status_dot_offline),
            status_dot_connecting: inherit_string(&self.status_dot_connecting, &base.status_dot_connecting),
            reaction_observed: inherit_string(&self.reaction_observed, &base.reaction_observed),
            reaction_dispatched: inherit_string(&self.reaction_dispatched, &base.reaction_dispatched),
            reaction_acted: inherit_string(&self.reaction_acted, &base.reaction_acted),
            reaction_unknown: inherit_string(&self.reaction_unknown, &base.reaction_unknown),
            sidebar_selected_bg: inherit_string(&self.sidebar_selected_bg, &base.sidebar_selected_bg),
            sidebar_selected_fg: inherit_string(&self.sidebar_selected_fg, &base.sidebar_selected_fg),
            sidebar_text: inherit_string(&self.sidebar_text, &base.sidebar_text),
            sidebar_border_focused: inherit_string(&self.sidebar_border_focused, &base.sidebar_border_focused),
            sidebar_border_unfocused: inherit_string(&self.sidebar_border_unfocused, &base.sidebar_border_unfocused),
            sidebar_item_bg: inherit_string(&self.sidebar_item_bg, &base.sidebar_item_bg),
            md_bold: inherit_string(&self.md_bold, &base.md_bold),
            md_italic: inherit_string(&self.md_italic, &base.md_italic),
            md_code: inherit_string(&self.md_code, &base.md_code),
            md_link: inherit_string(&self.md_link, &base.md_link),
            md_bullet: inherit_string(&self.md_bullet, &base.md_bullet),
            md_heading: inherit_string(&self.md_heading, &base.md_heading),
            md_rule: inherit_string(&self.md_rule, &base.md_rule),
            accent: inherit_string(&self.accent, &base.accent),
            border: inherit_string(&self.border, &base.border),
        }
    }
}

/// Parse a hex color like `"#ff00ff"` or `"ff00ff"` into `(r, g, b)`.
pub fn parse_hex(hex: &str) -> Option<(u8, u8, u8)> {
    let s = hex.strip_prefix('#').unwrap_or(hex);
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some((r, g, b))
}

/// The directory under the platform config root where user theme files live.
/// e.g. `~/.config/liberado/themes/` on Linux, `%APPDATA%\liberado\themes\` on Windows.
pub fn user_themes_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("liberado").join("themes"))
}

/// A collection of themes keyed by name. Built-in themes (dark, light) are always
/// present. User themes loaded from `<config>/liberado/themes/*.toml` can extend or
/// override built-in themes. A user theme that shares a name with a built-in replaces
/// it completely.
#[derive(Debug)]
pub struct ThemeRegistry {
    themes: HashMap<String, Theme>,
}

impl ThemeRegistry {
    /// Create a registry populated with the built-in dark and light themes.
    pub fn new() -> Self {
        let mut themes = HashMap::new();
        let dark = Theme::default_dark();
        let light = Theme::default_light();
        themes.insert(dark.name.clone(), dark);
        themes.insert(light.name.clone(), light);
        Self { themes }
    }

    /// Load user theme `.toml` files from a directory. Each file name (minus `.toml`)
    /// becomes the theme name (overriding the `name` field in the file). Themes that
    /// share a name with a built-in completely replace it.
    ///
    /// Malformed files are skipped with a warning logged (this crate has no `log`
    /// depedency — the caller should report errors).
    pub fn load_user_themes(&mut self, dir: &Path) -> Vec<LoadError> {
        let mut errors = Vec::new();
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return errors,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let file_stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown");
            match fs::read_to_string(&path) {
                Ok(contents) => match toml::from_str::<Theme>(&contents) {
                    Ok(mut theme) => {
                        theme.name = file_stem.to_string();
                        self.themes.insert(file_stem.to_string(), theme);
                    }
                    Err(e) => {
                        errors.push(LoadError {
                            path: path.clone(),
                            message: format!("invalid TOML: {e}"),
                        });
                    }
                },
                Err(e) => {
                    errors.push(LoadError {
                        path: path.clone(),
                        message: format!("could not read: {e}"),
                    });
                }
            }
        }
        errors
    }

    /// Reload user themes from disk, restoring built-ins to factory defaults first.
    pub fn reload(&mut self, dir: &Path) -> Vec<LoadError> {
        let dark = Theme::default_dark();
        let light = Theme::default_light();
        self.themes.insert("dark".into(), dark);
        self.themes.insert("light".into(), light);
        self.load_user_themes(dir)
    }

    /// Return sorted list of theme names.
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.themes.keys().map(|s| s.as_str()).collect();
        names.sort_unstable();
        names
    }

    /// Look up a theme by name. Returns `None` if not found.
    pub fn get(&self, name: &str) -> Option<&Theme> {
        self.themes.get(name)
    }

    /// Number of registered themes (built-in + user).
    pub fn len(&self) -> usize {
        self.themes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.themes.is_empty()
    }
}

impl Default for ThemeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Error reported when a user theme file fails to load.
#[derive(Debug, Clone)]
pub struct LoadError {
    pub path: PathBuf,
    pub message: String,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}

/// Embedded example themes that TOML authors can copy as starting points.
pub const EXAMPLE_SOLARIZED: &str = r##"name = "solarized"

# ── Chat pane ──
chat_user_prefix    = "#268bd2"
chat_user_text      = "#839496"
chat_assistant_text = "#657b83"
chat_system_text    = "#586e75"
chat_streaming_cursor = "#268bd2"

# ── Tool chips ──
tool_label  = "#b58900"
tool_name   = "#b58900"
tool_args   = "#586e75"
tool_ok     = "#859900"
tool_err    = "#dc322f"

# ── Code blocks ──
code_block_header = "#586e75"
code_block_bg     = "#002b36"
code_block_fg     = "#839496"

# ── Input line ──
input_border_focused   = "#268bd2"
input_border_unfocused = "#586e75"
input_placeholder      = "#586e75"
input_text             = "#839496"

# ── Status bar ──
status_bar_text    = "#586e75"
status_dot_online  = "#859900"
status_dot_offline = "#dc322f"
status_dot_connecting = "#b58900"

# ── Reactions ──
reaction_observed   = "#268bd2"
reaction_dispatched = "#b58900"
reaction_acted      = "#859900"
reaction_unknown    = "#586e75"

# ── Sidebar ──
sidebar_selected_bg      = "#268bd2"
sidebar_selected_fg      = "#002b36"
sidebar_text             = "#839496"
sidebar_border_focused   = "#268bd2"
sidebar_border_unfocused = "#586e75"
sidebar_item_bg         = "#002b36"

# ── Markdown ──
md_bold    = "#eee8d5"
md_italic  = "#839496"
md_code    = "#b58900"
md_link    = "#268bd2"
md_bullet  = "#268bd2"
md_heading = "#eee8d5"
md_rule    = "#586e75"

# ── General ──
accent         = "#268bd2"
border         = "#586e75"
focused_border = "#268bd2"
"##;

pub const EXAMPLE_GRUVBOX: &str = r##"name = "gruvbox"

# ── Chat pane ──
chat_user_prefix    = "#83a598"
chat_user_text      = "#ebdbb2"
chat_assistant_text = "#bdae93"
chat_system_text    = "#928374"
chat_streaming_cursor = "#83a598"

# ── Tool chips ──
tool_label  = "#fabd2f"
tool_name   = "#fabd2f"
tool_args   = "#928374"
tool_ok     = "#b8bb26"
tool_err    = "#fb4934"

# ── Code blocks ──
code_block_header = "#928374"
code_block_bg     = "#3c3836"
code_block_fg     = "#ebdbb2"

# ── Input line ──
input_border_focused   = "#83a598"
input_border_unfocused = "#504945"
input_placeholder      = "#504945"
input_text             = "#ebdbb2"

# ── Status bar ──
status_bar_text    = "#928374"
status_dot_online  = "#b8bb26"
status_dot_offline = "#fb4934"
status_dot_connecting = "#fabd2f"

# ── Reactions ──
reaction_observed   = "#83a598"
reaction_dispatched = "#fabd2f"
reaction_acted      = "#b8bb26"
reaction_unknown    = "#928374"

# ── Sidebar ──
sidebar_selected_bg      = "#83a598"
sidebar_selected_fg      = "#282828"
sidebar_text             = "#bdae93"
sidebar_border_focused   = "#83a598"
sidebar_border_unfocused = "#504945"
sidebar_item_bg         = "#1d2021"

# ── Markdown ──
md_bold    = "#ebdbb2"
md_italic  = "#bdae93"
md_code    = "#fabd2f"
md_link    = "#83a598"
md_bullet  = "#83a598"
md_heading = "#ebdbb2"
md_rule    = "#504945"

# ── General ──
accent         = "#83a598"
border         = "#504945"
focused_border = "#83a598"
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_hex_with_hash() {
        assert_eq!(parse_hex("#ff0000"), Some((255, 0, 0)));
    }

    #[test]
    fn parse_hex_without_hash() {
        assert_eq!(parse_hex("00ff00"), Some((0, 255, 0)));
    }

    #[test]
    fn parse_hex_invalid_is_none() {
        assert_eq!(parse_hex("zzz"), None);
        assert_eq!(parse_hex("#12345"), None);
    }

    #[test]
    fn resolve_uses_override() {
        let theme = Theme::default_dark();
        assert_eq!(theme.resolve(&theme.chat_user_text, "#fff"), "#ffffff");
    }

    #[test]
    fn resolve_falls_back() {
        let theme = Theme::default_dark();
        let result = theme.resolve(&None, "#fallback");
        assert_eq!(result, "#fallback");
    }

    #[test]
    fn default_dark_has_name() {
        assert_eq!(Theme::default_dark().name, "dark");
    }

    #[test]
    fn default_light_has_name() {
        assert_eq!(Theme::default_light().name, "light");
    }

    // ── Registry tests ──

    #[test]
    fn registry_has_builtins() {
        let reg = ThemeRegistry::new();
        assert!(reg.get("dark").is_some());
        assert!(reg.get("light").is_some());
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn registry_names_sorted() {
        let reg = ThemeRegistry::new();
        let names = reg.names();
        assert_eq!(names, vec!["dark", "light"]);
    }

    #[test]
    fn registry_loads_user_theme() {
        let dir = tempfile::tempdir().unwrap();
        let theme_path = dir.path().join("mocha.toml");
        let mut f = fs::File::create(&theme_path).unwrap();
        f.write_all(b"name = \"mocha\"\nchat_user_text = \"#ff0000\"\n").unwrap();
        drop(f);

        let mut reg = ThemeRegistry::new();
        reg.load_user_themes(dir.path());
        assert_eq!(reg.len(), 3);
        assert!(reg.get("mocha").is_some());
        assert_eq!(reg.get("mocha").unwrap().chat_user_text, Some("#ff0000".into()));
    }

    #[test]
    fn registry_user_theme_overrides_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let theme_path = dir.path().join("dark.toml");
        fs::write(&theme_path, "name = \"custom\"\nchat_user_text = \"#123456\"\n").unwrap();

        let mut reg = ThemeRegistry::new();
        reg.load_user_themes(dir.path());
        // User "dark.toml" overrides built-in dark
        assert!(reg.get("dark").is_some());
        assert_eq!(reg.get("dark").unwrap().chat_user_text, Some("#123456".into()));
    }

    #[test]
    fn registry_reload_restores_builtins() {
        let dir = tempfile::tempdir().unwrap();
        let theme_path = dir.path().join("dark.toml");
        fs::write(&theme_path, "chat_user_text = \"#ff0000\"\n").unwrap();

        let mut reg = ThemeRegistry::new();
        reg.load_user_themes(dir.path());
        assert_eq!(reg.get("dark").unwrap().chat_user_text, Some("#ff0000".into()));

        // Remove the file and reload — built-in dark comes back
        fs::remove_file(&theme_path).unwrap();
        reg.reload(dir.path());
        let dark = reg.get("dark").unwrap();
        assert_eq!(dark.chat_user_text, Some("#ffffff".into()));
    }

    #[test]
    fn registry_load_errors_reported() {
        let dir = tempfile::tempdir().unwrap();
        let theme_path = dir.path().join("bad.toml");
        fs::write(&theme_path, "not valid toml = =[").unwrap();

        let mut reg = ThemeRegistry::new();
        let errors = reg.load_user_themes(dir.path());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("invalid TOML"));
    }

    #[test]
    fn registry_empty_dir_no_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = ThemeRegistry::new();
        let errors = reg.load_user_themes(dir.path());
        assert!(errors.is_empty());
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn layered_on_fills_missing_tokens() {
        let base = Theme::default_dark();
        let partial = Theme {
            name: "partial".into(),
            chat_user_text: Some("#abcdef".into()),
            ..Default::default()
        };
        // Need a Default impl or use a constructor... actually Theme's Default would
        // have all None fields and empty name. But we don't have one.
        // Let me use a different approach — create a minimal theme manually.
        let layered = partial.layered_on(&base);
        assert_eq!(layered.name, "partial");
        assert_eq!(layered.chat_user_text, Some("#abcdef".into()));
        assert_eq!(layered.chat_user_prefix, base.chat_user_prefix);
        assert_eq!(layered.tool_ok, base.tool_ok);
    }
}
