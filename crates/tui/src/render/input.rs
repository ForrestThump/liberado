//! Renders the text-input area at the bottom of the TUI.
//!
//! Public entry point: `draw`. Internals are split into small, focused helpers:
//! * `build_block` — the bordered widget frame
//! * `draw_empty_placeholder` — rendered when the input buffer is empty
//! * `draw_content` — wraps + renders the actual text
//! * `wrap_input` — pre-splits logical lines into display-width segments
//! * `visual_cursor` — maps the byte cursor to a visual (line, column)
//! * `position_cursor` — tells the terminal where to blink

use liberado_theme::Theme;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{App, Focus};
use crate::ui::c;

// ── Public entry ─────────────────────────────────────────────────────

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, th: &Theme) {
    let input_bg = c(&th.input_bg, "#1a1a2e");
    let block = build_block(app, th);

    if app.input.is_empty() && app.focus == Focus::Input {
        draw_empty_placeholder(frame, area, block, th, input_bg);
        return;
    }

    let content_width = area.width.saturating_sub(2) as usize;
    // Ghost-complete: append dim suffix of the selected slash match (Grok Build–style).
    // Only when the cursor is at the end so mid-edit doesn't look wrong.
    let ghost = if app.focus == Focus::Input && app.cursor == app.input.len() {
        app.slash_ghost_suffix().unwrap_or_default()
    } else {
        String::new()
    };

    let wrapped = wrap_input_with_ghost(&app.input, &ghost, content_width, th, input_bg);
    let (mut cursor_line, cursor_col) = visual_cursor(&app.input, app.cursor, content_width);

    let max_content_rows = area.height.saturating_sub(2) as usize;
    let start = app.input_scroll;
    let end = (start + max_content_rows).min(wrapped.len());
    let visible: Vec<Line> = wrapped[start..end].to_vec();

    cursor_line = cursor_line.saturating_sub(app.input_scroll);

    let paragraph = Paragraph::new(visible)
        .block(block)
        .style(Style::default().bg(input_bg));

    frame.render_widget(paragraph, area);
    position_cursor(frame, area, app, cursor_line, cursor_col);
}

// ── Block / border ───────────────────────────────────────────────────

fn build_block(app: &App, th: &Theme) -> Block<'static> {
    let focused = app.focus == Focus::Input;
    let border_color = if focused {
        c(&th.input_border_focused, "#00ffff")
    } else {
        c(&th.input_border_unfocused, "#404040")
    };
    let streaming = if app.streaming { " [streaming…]" } else { "" };
    Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " Message{streaming} | / commands · Enter accept/send · Tab complete · Esc clear · Ctrl+C quit "
        ))
        .border_style(Style::default().fg(border_color))
}

// ── Empty-state ──────────────────────────────────────────────────────

fn draw_empty_placeholder(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    th: &Theme,
    input_bg: Color,
) {
    let placeholder_color = c(&th.input_placeholder, "#404040");
    let text = Line::from(Span::styled(
        "Type a message...",
        Style::default().fg(placeholder_color).bg(input_bg),
    ));
    let paragraph = Paragraph::new(text)
        .block(block)
        .style(Style::default().bg(input_bg));
    frame.render_widget(paragraph, area);
    frame.set_cursor_position((area.x + 1, area.y + 1));
}

// ── Cursor ───────────────────────────────────────────────────────────

fn position_cursor(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    cursor_visual_line: usize,
    cursor_visual_col: usize,
) {
    if app.focus != Focus::Input {
        return;
    }
    let max_visible_rows = area.height.saturating_sub(2) as usize;
    let clamped_line = cursor_visual_line.min(max_visible_rows.saturating_sub(1));
    let x = (area.x + 1 + cursor_visual_col as u16).min(area.right().saturating_sub(1));
    let y = (area.y + 1 + clamped_line as u16).min(area.bottom().saturating_sub(2));
    frame.set_cursor_position((x, y));
}

// ── Wrapping ─────────────────────────────────────────────────────────

/// Split typed input + optional ghost suffix into styled display lines.
/// Typed text uses normal input color; ghost uses placeholder/dim color.
fn wrap_input_with_ghost(
    typed: &str,
    ghost: &str,
    content_width: usize,
    th: &Theme,
    input_bg: Color,
) -> Vec<Line<'static>> {
    let input_fg = c(&th.input_text, "#ffffff");
    let ghost_fg = c(&th.input_placeholder, "#404040");
    let typed_style = Style::default().fg(input_fg).bg(input_bg);
    let ghost_style = Style::default().fg(ghost_fg).bg(input_bg);

    // Single-line slash prompts are the common case; multi-line keeps typed only on wraps.
    let combined = if ghost.is_empty() {
        typed.to_string()
    } else {
        format!("{typed}{ghost}")
    };
    let typed_chars = typed.chars().count();

    let mut out: Vec<Line> = Vec::new();
    let mut char_offset = 0usize;
    for logical in combined.lines() {
        push_wrapped_styled(
            &mut out,
            logical,
            content_width,
            char_offset,
            typed_chars,
            typed_style,
            ghost_style,
        );
        char_offset += logical.chars().count() + 1; // +1 for newline in combined
    }
    if out.is_empty() {
        out.push(Line::from(Span::styled(String::new(), typed_style)));
    }
    out
}

/// Push one logical line, splitting spans so typed vs ghost stay correctly colored across wraps.
fn push_wrapped_styled(
    out: &mut Vec<Line<'static>>,
    logical: &str,
    content_width: usize,
    line_start_char: usize,
    typed_chars: usize,
    typed_style: Style,
    ghost_style: Style,
) {
    if content_width == 0 {
        out.push(style_segment(
            logical,
            line_start_char,
            typed_chars,
            typed_style,
            ghost_style,
        ));
        return;
    }
    let mut remaining = logical;
    let mut local_char = 0usize;
    loop {
        let (segment, rest) = take_width(remaining, content_width);
        out.push(style_segment(
            segment,
            line_start_char + local_char,
            typed_chars,
            typed_style,
            ghost_style,
        ));
        local_char += segment.chars().count();
        if rest.is_empty() {
            break;
        }
        remaining = rest;
    }
}

fn style_segment(
    segment: &str,
    start_char: usize,
    typed_chars: usize,
    typed_style: Style,
    ghost_style: Style,
) -> Line<'static> {
    if segment.is_empty() {
        return Line::from(Span::styled(String::new(), typed_style));
    }
    let end_char = start_char + segment.chars().count();
    if end_char <= typed_chars {
        return Line::from(Span::styled(segment.to_string(), typed_style));
    }
    if start_char >= typed_chars {
        return Line::from(Span::styled(segment.to_string(), ghost_style));
    }
    // Split mid-segment: typed prefix + ghost suffix.
    let split = typed_chars - start_char;
    let mut chars = segment.chars();
    let typed_part: String = chars.by_ref().take(split).collect();
    let ghost_part: String = chars.collect();
    Line::from(vec![
        Span::styled(typed_part, typed_style),
        Span::styled(ghost_part, ghost_style),
    ])
}

/// Return the prefix of `s` containing at most `n` characters (on
/// grapheme boundaries), plus the remainder.
fn take_width(s: &str, n: usize) -> (&str, &str) {
    let mut byte = 0usize;
    for (count, (i, c)) in s.char_indices().enumerate() {
        if count >= n {
            return (&s[..byte], &s[byte..]);
        }
        byte = i + c.len_utf8();
    }
    // Entire string fits.
    (s, "")
}

/// Map a byte cursor into `input` to a (visual_line, visual_column)
/// pair, accounting for wrapping at `content_width`.
fn visual_cursor(input: &str, cursor: usize, content_width: usize) -> (usize, usize) {
    let mut visual_line = 0usize;
    let mut byte_pos = 0usize;

    for logical in input.lines() {
        let line_end = byte_pos + logical.len();

        if cursor <= line_end {
            let column = input[byte_pos..cursor].chars().count();
            visual_line += column / content_width.max(1);
            return (visual_line, column % content_width.max(1));
        }

        // Advance past this logical line + its '\n'.
        let chars = logical.chars().count();
        visual_line += visual_lines_for_offset(chars, content_width);
        byte_pos = (line_end + 1).min(input.len());
    }
    (visual_line, 0)
}

/// How many display lines `char_count` characters occupy at `content_width`.
fn visual_lines_for_offset(chars: usize, content_width: usize) -> usize {
    if content_width == 0 || chars == 0 {
        1
    } else {
        chars.div_ceil(content_width)
    }
}
