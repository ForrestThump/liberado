//! Emit CSS custom properties from a [`liberado_theme::Theme`].
//!
//! Call [`theme_css_vars`] to produce a `:root { … }` block that wires every
//! theme token into the stylesheet as a `--lib-*` CSS variable.  The webui
//! injects this block before `main.css` so every selector can use `var(--lib-*)`.
//!
//! Switching themes at runtime is trivial: call this function with the new
//! `Theme` and swap the injected `<style>` element's text content.

use liberado_theme::{Theme, ThemeRegistry};

/// Where the chosen theme name is remembered.
///
/// The TUI persists to `settings.toml` under the platform config dir (see
/// `liberado_theme::save_theme_preference`). A browser cannot write there, so this is the same idea
/// in the only store it has. That keeps the *intent* of `UiSettings` intact — a theme is a
/// machine-local UI preference, not a vault artifact — at the cost of not sharing the choice with
/// the TUI on the same machine.
const STORAGE_KEY: &str = "liberado.theme";

/// The theme to render, by name, falling back to the built-in dark when the name is unknown.
///
/// Resolved through the **shared** [`ThemeRegistry`] rather than a webui-local list, so `dark`,
/// `light` and `nord` are exactly the set the TUI has and adding a built-in upstream reaches both
/// surfaces with no change here.
///
/// Not shared with the TUI: user theme *files*. The registry loads those from
/// `<config>/liberado/themes/*.toml`, which a WASM build cannot read — see `theme_names` in
/// `components/slash_commands.rs`.
pub fn theme_by_name(name: &str) -> Theme {
    ThemeRegistry::new()
        .get(name)
        .cloned()
        .unwrap_or_else(Theme::default_dark)
}

/// Every theme name this surface can render, sorted for a stable `/theme list`.
pub fn theme_names() -> Vec<String> {
    let registry = ThemeRegistry::new();
    let mut names: Vec<String> = registry.names().into_iter().map(str::to_string).collect();
    names.sort();
    names
}

/// The remembered theme name, or `dark` when nothing is stored yet.
pub fn saved_theme_name() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(name) = web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .and_then(|s| s.get_item(STORAGE_KEY).ok().flatten())
            .filter(|n| !n.trim().is_empty())
        {
            return name;
        }
    }
    "dark".to_string()
}

/// Remember `name` for the next load. Best-effort: a browser with storage disabled just means the
/// choice lasts for the session, which is better than refusing to switch at all.
pub fn save_theme_name(name: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
            let _ = storage.set_item(STORAGE_KEY, name);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = name;
    }
}

/// Return a `:root { … }` CSS block defining every `--lib-*` custom property
/// from `t`.  Any `None` field falls back to the corresponding value in
/// `Theme::default_dark()`.
pub fn theme_css_vars(t: &Theme) -> String {
    let d = Theme::default_dark();
    let r = |v: &Option<String>, fb: &str| -> String {
        v.clone().unwrap_or_else(|| {
            // Try the matching default_dark field via the `fb` fallback constant.
            fb.to_string()
        })
    };

    // Pull all tokens — for None fields fall back to default_dark equivalents.
    let app_bg = r(&t.app_bg, d.app_bg.as_deref().unwrap_or("#0d0d1a"));
    let accent = r(&t.accent, d.accent.as_deref().unwrap_or("#00ffff"));
    let border = r(&t.border, d.border.as_deref().unwrap_or("#808080"));

    let chat_user_text = r(
        &t.chat_user_text,
        d.chat_user_text.as_deref().unwrap_or("#ffffff"),
    );
    let chat_user_prefix = r(
        &t.chat_user_prefix,
        d.chat_user_prefix.as_deref().unwrap_or("#00ffff"),
    );
    let chat_assistant_text = r(
        &t.chat_assistant_text,
        d.chat_assistant_text.as_deref().unwrap_or("#c0c0c0"),
    );
    let chat_system_text = r(
        &t.chat_system_text,
        d.chat_system_text.as_deref().unwrap_or("#808080"),
    );
    let chat_streaming_cursor = r(
        &t.chat_streaming_cursor,
        d.chat_streaming_cursor.as_deref().unwrap_or("#00ffff"),
    );

    let tool_label = r(&t.tool_label, d.tool_label.as_deref().unwrap_or("#ffff00"));
    let tool_name = r(&t.tool_name, d.tool_name.as_deref().unwrap_or("#ffff00"));
    let tool_args = r(&t.tool_args, d.tool_args.as_deref().unwrap_or("#808080"));
    let tool_ok = r(&t.tool_ok, d.tool_ok.as_deref().unwrap_or("#00ff00"));
    let tool_err = r(&t.tool_err, d.tool_err.as_deref().unwrap_or("#ff0000"));

    let code_block_header = r(
        &t.code_block_header,
        d.code_block_header.as_deref().unwrap_or("#808000"),
    );
    let code_block_bg = r(
        &t.code_block_bg,
        d.code_block_bg.as_deref().unwrap_or("#303030"),
    );
    let code_block_fg = r(
        &t.code_block_fg,
        d.code_block_fg.as_deref().unwrap_or("#c0c0c0"),
    );

    let input_bg = r(&t.input_bg, d.input_bg.as_deref().unwrap_or("#1a1a2e"));
    let input_text = r(&t.input_text, d.input_text.as_deref().unwrap_or("#ffffff"));
    let input_placeholder = r(
        &t.input_placeholder,
        d.input_placeholder.as_deref().unwrap_or("#404040"),
    );
    let input_border_focused = r(
        &t.input_border_focused,
        d.input_border_focused.as_deref().unwrap_or("#00ffff"),
    );
    let input_border_unfocused = r(
        &t.input_border_unfocused,
        d.input_border_unfocused.as_deref().unwrap_or("#404040"),
    );

    let status_bar_text = r(
        &t.status_bar_text,
        d.status_bar_text.as_deref().unwrap_or("#808080"),
    );
    let status_dot_online = r(
        &t.status_dot_online,
        d.status_dot_online.as_deref().unwrap_or("#00ff00"),
    );
    let status_dot_offline = r(
        &t.status_dot_offline,
        d.status_dot_offline.as_deref().unwrap_or("#ff0000"),
    );
    let status_dot_connecting = r(
        &t.status_dot_connecting,
        d.status_dot_connecting.as_deref().unwrap_or("#ffff00"),
    );

    let reaction_observed = r(
        &t.reaction_observed,
        d.reaction_observed.as_deref().unwrap_or("#00ffff"),
    );
    let reaction_dispatched = r(
        &t.reaction_dispatched,
        d.reaction_dispatched.as_deref().unwrap_or("#ffff00"),
    );
    let reaction_acted = r(
        &t.reaction_acted,
        d.reaction_acted.as_deref().unwrap_or("#00ff00"),
    );
    let reaction_unknown = r(
        &t.reaction_unknown,
        d.reaction_unknown.as_deref().unwrap_or("#808080"),
    );

    let sidebar_selected_bg = r(
        &t.sidebar_selected_bg,
        d.sidebar_selected_bg.as_deref().unwrap_or("#00ffff"),
    );
    let sidebar_selected_fg = r(
        &t.sidebar_selected_fg,
        d.sidebar_selected_fg.as_deref().unwrap_or("#000000"),
    );
    let sidebar_text = r(
        &t.sidebar_text,
        d.sidebar_text.as_deref().unwrap_or("#c0c0c0"),
    );
    let sidebar_border_focused = r(
        &t.sidebar_border_focused,
        d.sidebar_border_focused.as_deref().unwrap_or("#00ffff"),
    );
    let sidebar_border_unfocused = r(
        &t.sidebar_border_unfocused,
        d.sidebar_border_unfocused.as_deref().unwrap_or("#808080"),
    );
    let sidebar_item_bg = r(
        &t.sidebar_item_bg,
        d.sidebar_item_bg.as_deref().unwrap_or("#101010"),
    );

    let md_bold = r(&t.md_bold, d.md_bold.as_deref().unwrap_or("#ffffff"));
    let md_italic = r(&t.md_italic, d.md_italic.as_deref().unwrap_or("#c0c0c0"));
    let md_code = r(&t.md_code, d.md_code.as_deref().unwrap_or("#ffff00"));
    let md_link = r(&t.md_link, d.md_link.as_deref().unwrap_or("#8080ff"));
    let md_bullet = r(&t.md_bullet, d.md_bullet.as_deref().unwrap_or("#00ffff"));
    let md_heading = r(&t.md_heading, d.md_heading.as_deref().unwrap_or("#ffffff"));
    let md_rule = r(&t.md_rule, d.md_rule.as_deref().unwrap_or("#404040"));

    // Derived structural vars — computed from existing tokens, not new theme fields.
    // --lib-surface:   panel / assistant-bubble background (sidebar_item_bg)
    // --lib-surface-2: code-block / tool-chip background  (code_block_bg)
    let surface = sidebar_item_bg.clone();
    let surface2 = code_block_bg.clone();

    format!(
        r#":root {{
  /* ── General ─────────────────────────────── */
  --lib-app-bg: {app_bg};
  --lib-accent: {accent};
  --lib-border: {border};

  /* ── Derived surfaces ────────────────────── */
  --lib-surface: {surface};
  --lib-surface-2: {surface2};

  /* ── Chat pane ───────────────────────────── */
  --lib-chat-user-text: {chat_user_text};
  --lib-chat-user-prefix: {chat_user_prefix};
  --lib-chat-assistant-text: {chat_assistant_text};
  --lib-chat-system-text: {chat_system_text};
  --lib-chat-streaming-cursor: {chat_streaming_cursor};

  /* ── Tool chips ──────────────────────────── */
  --lib-tool-label: {tool_label};
  --lib-tool-name: {tool_name};
  --lib-tool-args: {tool_args};
  --lib-tool-ok: {tool_ok};
  --lib-tool-err: {tool_err};

  /* ── Code blocks ─────────────────────────── */
  --lib-code-block-header: {code_block_header};
  --lib-code-block-bg: {code_block_bg};
  --lib-code-block-fg: {code_block_fg};

  /* ── Input line ──────────────────────────── */
  --lib-input-bg: {input_bg};
  --lib-input-text: {input_text};
  --lib-input-placeholder: {input_placeholder};
  --lib-input-border-focused: {input_border_focused};
  --lib-input-border-unfocused: {input_border_unfocused};

  /* ── Status bar ──────────────────────────── */
  --lib-status-bar-text: {status_bar_text};
  --lib-status-dot-online: {status_dot_online};
  --lib-status-dot-offline: {status_dot_offline};
  --lib-status-dot-connecting: {status_dot_connecting};

  /* ── Reactions ───────────────────────────── */
  --lib-reaction-observed: {reaction_observed};
  --lib-reaction-dispatched: {reaction_dispatched};
  --lib-reaction-acted: {reaction_acted};
  --lib-reaction-unknown: {reaction_unknown};

  /* ── Sidebar ─────────────────────────────── */
  --lib-sidebar-selected-bg: {sidebar_selected_bg};
  --lib-sidebar-selected-fg: {sidebar_selected_fg};
  --lib-sidebar-text: {sidebar_text};
  --lib-sidebar-border-focused: {sidebar_border_focused};
  --lib-sidebar-border-unfocused: {sidebar_border_unfocused};
  --lib-sidebar-item-bg: {sidebar_item_bg};

  /* ── Markdown ────────────────────────────── */
  --lib-md-bold: {md_bold};
  --lib-md-italic: {md_italic};
  --lib-md-code: {md_code};
  --lib-md-link: {md_link};
  --lib-md-bullet: {md_bullet};
  --lib-md-heading: {md_heading};
  --lib-md-rule: {md_rule};
}}"#
    )
}
