//! Renders the text-input area at the bottom of the TUI.
//!
//! Public entry point: [`draw`]. Internals are split into small, focused helpers:
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
    let wrapped = wrap_input(&app.input, content_width);
    let (cursor_line, cursor_col) = visual_cursor(&app.input, app.cursor, content_width);

    let input_fg = c(&th.input_text, "#ffffff");
    // Apply colours to each pre-wrapped line.
    let styled: Vec<Line> = wrapped
        .into_iter()
        .map(|line| {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            Line::from(Span::styled(
                text,
                Style::default().fg(input_fg).bg(input_bg),
            ))
        })
        .collect();

    let paragraph = Paragraph::new(styled)
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
            " Message{streaming} | Enter to send, Shift+Enter for newline, Esc to clear | Ctrl+C clear/quit "
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
    let x = (area.x + 1 + cursor_visual_col as u16).min(area.right().saturating_sub(1));
    let y = (area.y + 1 + cursor_visual_line as u16).min(area.bottom().saturating_sub(2));
    frame.set_cursor_position((x, y));
}

// ── Wrapping ─────────────────────────────────────────────────────────

/// Split `input` into display lines, wrapping each logical line at
/// `content_width` characters so the Paragraph never needs to wrap on
/// its own (we control height exactly).
fn wrap_input(input: &str, content_width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line> = Vec::new();
    for logical in input.lines() {
        push_wrapped_line(&mut out, logical, content_width);
    }
    if out.is_empty() {
        out.push(Line::from(Span::raw("")));
    }
    out
}

/// Push one logical line into `out`, splitting it into segments no wider
/// than `content_width` characters.
fn push_wrapped_line(out: &mut Vec<Line<'static>>, logical: &str, content_width: usize) {
    if content_width == 0 {
        out.push(Line::from(Span::raw(logical.to_string())));
        return;
    }
    let mut remaining = logical;
    loop {
        let (segment, rest) = take_width(remaining, content_width);
        out.push(Line::from(Span::raw(segment.to_string())));
        if rest.is_empty() {
            break;
        }
        remaining = rest;
    }
}

/// Return the prefix of `s` containing at most `n` characters (on
/// grapheme boundaries), plus the remainder.
fn take_width(s: &str, n: usize) -> (&str, &str) {
    let mut count = 0usize;
    let mut byte = 0usize;
    for (i, c) in s.char_indices() {
        if count >= n {
            return (&s[..byte], &s[byte..]);
        }
        count += 1;
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
            visual_line += visual_lines_for_offset(column, content_width);
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
    if content_width == 0 {
        1
    } else if chars == 0 {
        1
    } else {
        (chars + content_width - 1) / content_width
    }
}
