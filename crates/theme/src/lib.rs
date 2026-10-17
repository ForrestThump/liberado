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

mod themes;

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
    /// Subtle background behind user turns so they read apart from agent output.
    #[serde(default)]
    pub chat_user_bg: Option<String>,
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

    // ── Input line ─────────────────────────────────────────
    #[serde(default)]
    pub input_bg: Option<String>,

    // ── General ────────────────────────────────────────────
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub border: Option<String>,

    /// The terminal background fill used to clear the frame before drawing.
    /// Defaults to black for dark themes, white for light themes.
    #[serde(default)]
    pub app_bg: Option<String>,
}

impl Theme {
    /// Resolve a color token: return the override if `Some`, otherwise the `fallback` hex
    /// string.
    pub fn resolve(&self, value: &Option<String>, fallback: &str) -> String {
        value.clone().unwrap_or_else(|| fallback.to_string())
    }

    /// Fill in any `None` tokens from `base`. The returned theme has every token from
    /// `self`, with missing tokens inherited from `base`. Name comes from `self`.
    pub fn layered_on(&self, base: &Theme) -> Theme {
        let inherit_string =
            |a: &Option<String>, b: &Option<String>| a.clone().or_else(|| b.clone());
        Theme {
            name: self.name.clone(),
            chat_user_prefix: inherit_string(&self.chat_user_prefix, &base.chat_user_prefix),
            chat_user_text: inherit_string(&self.chat_user_text, &base.chat_user_text),
            chat_user_bg: inherit_string(&self.chat_user_bg, &base.chat_user_bg),
            chat_assistant_text: inherit_string(
                &self.chat_assistant_text,
                &base.chat_assistant_text,
            ),
            chat_system_text: inherit_string(&self.chat_system_text, &base.chat_system_text),
            chat_streaming_cursor: inherit_string(
                &self.chat_streaming_cursor,
                &base.chat_streaming_cursor,
            ),
            tool_label: inherit_string(&self.tool_label, &base.tool_label),
            tool_name: inherit_string(&self.tool_name, &base.tool_name),
            tool_args: inherit_string(&self.tool_args, &base.tool_args),
            tool_ok: inherit_string(&self.tool_ok, &base.tool_ok),
            tool_err: inherit_string(&self.tool_err, &base.tool_err),
            code_block_header: inherit_string(&self.code_block_header, &base.code_block_header),
            code_block_bg: inherit_string(&self.code_block_bg, &base.code_block_bg),
            code_block_fg: inherit_string(&self.code_block_fg, &base.code_block_fg),
            input_border_focused: inherit_string(
                &self.input_border_focused,
                &base.input_border_focused,
            ),
            input_border_unfocused: inherit_string(
                &self.input_border_unfocused,
                &base.input_border_unfocused,
            ),
            input_placeholder: inherit_string(&self.input_placeholder, &base.input_placeholder),
            input_text: inherit_string(&self.input_text, &base.input_text),
            input_bg: inherit_string(&self.input_bg, &base.input_bg),
            status_bar_text: inherit_string(&self.status_bar_text, &base.status_bar_text),
            status_dot_online: inherit_string(&self.status_dot_online, &base.status_dot_online),
            status_dot_offline: inherit_string(&self.status_dot_offline, &base.status_dot_offline),
            status_dot_connecting: inherit_string(
                &self.status_dot_connecting,
                &base.status_dot_connecting,
            ),
            reaction_observed: inherit_string(&self.reaction_observed, &base.reaction_observed),
            reaction_dispatched: inherit_string(
                &self.reaction_dispatched,
                &base.reaction_dispatched,
            ),
            reaction_acted: inherit_string(&self.reaction_acted, &base.reaction_acted),
            reaction_unknown: inherit_string(&self.reaction_unknown, &base.reaction_unknown),
            sidebar_selected_bg: inherit_string(
                &self.sidebar_selected_bg,
                &base.sidebar_selected_bg,
            ),
            sidebar_selected_fg: inherit_string(
                &self.sidebar_selected_fg,
                &base.sidebar_selected_fg,
            ),
            sidebar_text: inherit_string(&self.sidebar_text, &base.sidebar_text),
            sidebar_border_focused: inherit_string(
                &self.sidebar_border_focused,
                &base.sidebar_border_focused,
            ),
            sidebar_border_unfocused: inherit_string(
                &self.sidebar_border_unfocused,
                &base.sidebar_border_unfocused,
            ),
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
            app_bg: inherit_string(&self.app_bg, &base.app_bg),
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

/// Platform config dir for Liberado client prefs (same root as themes, one level up).
/// e.g. `~/.config/liberado/` / `%APPDATA%\liberado\`.
pub fn user_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("liberado"))
}

/// Path to the client UI settings file (`settings.toml` next to `themes/`).
pub fn user_settings_path() -> Option<PathBuf> {
    user_config_dir().map(|d| d.join("settings.toml"))
}

/// Persistent client UI preferences (TUI today; WebUI can share later).
///
/// Lives under the platform config dir — not the daemon vault or `.liberado` data dir —
/// so theme choice is a machine-local UI preference, not a vault artifact.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiSettings {
    /// Preferred theme name (must exist in the registry when applied).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
}

/// Load UI settings from disk. Missing file → empty defaults (caller picks built-in theme).
pub fn load_ui_settings() -> UiSettings {
    let Some(path) = user_settings_path() else {
        return UiSettings::default();
    };
    load_ui_settings_from(&path)
}

/// Pure path-based load (tests).
pub fn load_ui_settings_from(path: &Path) -> UiSettings {
    match fs::read_to_string(path) {
        Ok(contents) => toml::from_str(&contents).unwrap_or_default(),
        Err(_) => UiSettings::default(),
    }
}

/// Persist the active theme name into `settings.toml`, preserving other fields.
pub fn save_theme_preference(theme_name: &str) -> Result<(), String> {
    let path = user_settings_path().ok_or_else(|| "could not resolve config dir".to_string())?;
    save_theme_preference_to(&path, theme_name)
}

/// Pure path-based save (tests).
pub fn save_theme_preference_to(path: &Path, theme_name: &str) -> Result<(), String> {
    let mut settings = load_ui_settings_from(path);
    settings.theme = Some(theme_name.trim().to_string());
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create config dir: {e}"))?;
    }
    let body = toml::to_string_pretty(&settings).map_err(|e| format!("serialize settings: {e}"))?;
    fs::write(path, body).map_err(|e| format!("write settings: {e}"))
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
    /// Create a registry populated with the built-in themes (dark, light, nord, sunset,
    /// forest, plum, mono).
    pub fn new() -> Self {
        let mut themes = HashMap::new();
        for theme in Theme::builtins() {
            themes.insert(theme.name.clone(), theme);
        }
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
        for theme in Theme::builtins() {
            self.themes.insert(theme.name.clone(), theme);
        }
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

    #[test]
    fn default_nord_has_name_and_polar_night_bg() {
        let t = Theme::default_nord();
        assert_eq!(t.name, "nord");
        assert_eq!(t.app_bg.as_deref(), Some("#2e3440")); // nord0
        assert_eq!(t.accent.as_deref(), Some("#88c0d0")); // nord8 frost ice
        assert_eq!(t.tool_err.as_deref(), Some("#bf616a")); // nord11 aurora red
        assert_eq!(t.tool_ok.as_deref(), Some("#a3be8c")); // nord14 aurora green
    }

    // ── Registry tests ──

    #[test]
    fn registry_has_builtins() {
        let reg = ThemeRegistry::new();
        let builtins = Theme::builtins();
        for theme in &builtins {
            assert!(
                reg.get(&theme.name).is_some(),
                "missing built-in {}",
                theme.name
            );
        }
        assert_eq!(reg.len(), builtins.len());
    }

    #[test]
    fn registry_names_sorted() {
        let reg = ThemeRegistry::new();
        let builtins = Theme::builtins();
        let mut expected: Vec<&str> = builtins.iter().map(|theme| theme.name.as_str()).collect();
        expected.sort_unstable();
        assert_eq!(reg.names(), expected);
    }

    #[test]
    fn registry_loads_user_theme() {
        let dir = tempfile::tempdir().unwrap();
        let theme_path = dir.path().join("mocha.toml");
        let mut f = fs::File::create(&theme_path).unwrap();
        f.write_all(b"name = \"mocha\"\nchat_user_text = \"#ff0000\"\n")
            .unwrap();
        drop(f);

        let mut reg = ThemeRegistry::new();
        reg.load_user_themes(dir.path());
        assert_eq!(reg.len(), Theme::builtins().len() + 1); // builtins + mocha
        assert!(reg.get("mocha").is_some());
        assert_eq!(
            reg.get("mocha").unwrap().chat_user_text,
            Some("#ff0000".into())
        );
    }

    #[test]
    fn registry_user_theme_overrides_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let theme_path = dir.path().join("dark.toml");
        fs::write(
            &theme_path,
            "name = \"custom\"\nchat_user_text = \"#123456\"\n",
        )
        .unwrap();

        let mut reg = ThemeRegistry::new();
        reg.load_user_themes(dir.path());
        // User "dark.toml" overrides built-in dark
        assert!(reg.get("dark").is_some());
        assert_eq!(
            reg.get("dark").unwrap().chat_user_text,
            Some("#123456".into())
        );
    }

    #[test]
    fn registry_reload_restores_builtins() {
        let dir = tempfile::tempdir().unwrap();
        let theme_path = dir.path().join("dark.toml");
        fs::write(&theme_path, "chat_user_text = \"#ff0000\"\n").unwrap();

        let mut reg = ThemeRegistry::new();
        reg.load_user_themes(dir.path());
        assert_eq!(
            reg.get("dark").unwrap().chat_user_text,
            Some("#ff0000".into())
        );

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
        assert_eq!(reg.len(), Theme::builtins().len());
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

    #[test]
    fn ui_settings_round_trip_theme_preference() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        assert!(load_ui_settings_from(&path).theme.is_none());
        save_theme_preference_to(&path, "nord").unwrap();
        assert_eq!(load_ui_settings_from(&path).theme.as_deref(), Some("nord"));
        save_theme_preference_to(&path, "light").unwrap();
        assert_eq!(load_ui_settings_from(&path).theme.as_deref(), Some("light"));
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("theme"));
        assert!(body.contains("light"));
    }
}

#[cfg(test)]
#[path = "theme_survivor_tests.rs"]
mod survivor_tests;
