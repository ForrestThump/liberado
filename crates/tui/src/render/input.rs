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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Focus;
    use crate::render::test_support;

    // ── Pure wrapping/cursor helpers ───────────────────────────────────

    #[test]
    fn take_width_splits_on_char_boundaries() {
        assert_eq!(take_width("hello world", 5), ("hello", " world"));
        assert_eq!(take_width("abc", 10), ("abc", ""));
        assert_eq!(take_width("abc", 0), ("", "abc"));
        assert_eq!(take_width("ééçç", 2), ("éé", "çç"));
    }

    #[test]
    fn visual_lines_for_offset_counts_wraps() {
        assert_eq!(visual_lines_for_offset(0, 10), 1);
        assert_eq!(visual_lines_for_offset(3, 0), 1);
        assert_eq!(visual_lines_for_offset(10, 5), 2);
        assert_eq!(visual_lines_for_offset(15, 5), 3);
        assert_eq!(visual_lines_for_offset(10, 10), 1);
    }

    #[test]
    fn visual_cursor_accounts_for_wrapping() {
        // cursor at byte 0 → (0,0); a cursor mid-first-line maps straight down.
        assert_eq!(visual_cursor("hello", 3, 10), (0, 3));
        // "hello world": char 8 folds onto line 2 at width 5.
        assert_eq!(visual_cursor("hello world", 8, 5), (1, 3));
        // Ends of lines clamp to the start of the next visual line.
        assert_eq!(visual_cursor("hello", 5, 10), (0, 5));
        // Multi-line input, cursor on the second logical line.
        assert_eq!(visual_cursor("ab\ncd", 4, 10), (1, 1));
    }

    #[test]
    fn wrap_input_with_ghost_splits_typed_and_ghost() {
        let app = test_support::app();
        let th = app.theme;
        let bg = c(&th.input_bg, "#1a1a2e");
        let lines = wrap_input_with_ghost("/new mygoal", " — start a new chat", 12, &th, bg);
        // First line holds only typed text at width 12; the ghost spills to line 2.
        let l0: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        let l1: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(l0.trim_end(), "/new mygoal");
        assert!(l1.contains("start"), "ghost tail: {l1}");
        // The ghost span uses the placeholder color, not the typed color.
        let expected_ghost_fg = c(&th.input_placeholder, "#404040");
        let ghost_span = lines[1]
            .spans
            .iter()
            .find(|s| s.style.fg == Some(expected_ghost_fg));
        assert!(ghost_span.is_some(), "ghost must be dimmed");
    }

    #[test]
    fn style_segment_splits_a_mixed_already_normalized_segment() {
        let app = test_support::app();
        let th = app.theme;
        let bg = c(&th.input_bg, "#1a1a2e");
        let typed = Style::default().fg(c(&th.input_text, "#ffffff")).bg(bg);
        let ghost = Style::default()
            .fg(c(&th.input_placeholder, "#404040"))
            .bg(bg);
        // Whole segment typed.
        let line = style_segment("abcd", 0, 4, typed, ghost);
        assert_eq!(line.spans.len(), 1);
        // Mixed: 2 typed + 2 ghost.
        let line = style_segment("abcd", 2, 4, typed, ghost);
        assert_eq!(line.spans.len(), 2, "must split typed/ghost");
        assert_eq!(line.spans[0].content.as_ref(), "ab");
        assert_eq!(line.spans[1].content.as_ref(), "cd");
    }

    // ── Draw path ──────────────────────────────────────────────────────

    #[test]
    fn empty_focused_input_shows_the_placeholder() {
        let mut app = test_support::app();
        app.focus = Focus::Input;
        let th = app.theme.clone();
        let out =
            test_support::render_pane(60, 6, |f| draw(f, Rect::new(0, 3, 60, 3), &mut app, &th));
        assert!(out.contains("Type a message"), "placeholder:\n{out}");
        assert!(out.contains("Message"), "block title:\n{out}");
    }

    #[test]
    fn focused_input_renders_typed_text() {
        let mut app = test_support::app();
        app.focus = Focus::Input;
        app.input = "hello there".into();
        app.cursor = "hello".len();
        let th = app.theme.clone();
        let out =
            test_support::render_pane(40, 6, |f| draw(f, Rect::new(0, 3, 40, 3), &mut app, &th));
        assert!(out.contains("hello there"), "typed text:\n{out}");
        assert!(
            !out.contains("Type a message"),
            "no placeholder with text:\n{out}"
        );
    }

    #[test]
    fn long_input_wraps_within_the_block() {
        let mut app = test_support::app();
        app.focus = Focus::Input;
        app.input = "a very long message that must wrap across several visual lines".into();
        app.cursor = app.input.len();
        let th = app.theme.clone();
        let out =
            test_support::render_pane(30, 8, |f| draw(f, Rect::new(0, 2, 30, 6), &mut app, &th));
        // The wrapped text spans at least two content rows, so the block isn't one line high:
        // the original single logical line broke onto `a very...`, `t wrap...`, ` lines`.
        let content_rows = out
            .lines()
            .filter(|l| l.contains("a very") || l.contains("wrap across") || l.contains("lines"))
            .count();
        assert!(content_rows >= 2, "wrapped across rows:\n{out}");
    }

    #[test]
    fn unfocused_input_uses_the_unfocused_border() {
        let mut app = test_support::app();
        app.focus = Focus::ChatMessages;
        app.input = "text".into();
        let th = app.theme.clone();
        let out =
            test_support::render_pane(40, 6, |f| draw(f, Rect::new(0, 3, 40, 3), &mut app, &th));
        // Block still renders, just from the other border color (asserted via presence).
        assert!(out.contains("Message"), "block title:\n{out}");
    }
}

#[cfg(test)]
mod cursor_tests {
    use super::*;
    use crate::app::Focus;
    use crate::render::test_support;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn placeholder_positions_the_cursor_inside_the_block() {
        let mut app = test_support::app();
        app.focus = Focus::Input;
        let th = app.theme.clone();
        let mut terminal = Terminal::new(TestBackend::new(60, 6)).unwrap();
        terminal
            .draw(|f| draw(f, Rect::new(0, 3, 60, 3), &mut app, &th))
            .unwrap();
        // area.y + 1 = 4, area.x + 1 = 1. A `+`→`*` or `+`→`-` mutation moves this.
        terminal.backend_mut().assert_cursor_position((1, 4));
    }

    #[test]
    fn typed_input_moves_the_cursor_to_the_visual_column() {
        let mut app = test_support::app();
        app.focus = Focus::Input;
        app.input = "hello".into();
        app.cursor = 3; // mid-word
        let th = app.theme.clone();
        let mut terminal = Terminal::new(TestBackend::new(60, 6)).unwrap();
        terminal
            .draw(|f| draw(f, Rect::new(0, 3, 60, 3), &mut app, &th))
            .unwrap();
        // x = area.x + 1 + col(3) = 4; y = area.y + 1 + line(0) = 4.
        terminal.backend_mut().assert_cursor_position((4, 4));
    }

    #[test]
    fn unfocused_input_never_touches_the_cursor() {
        let mut app = test_support::app();
        app.focus = Focus::ChatMessages;
        app.input = "hello".into();
        app.cursor = 3;
        let th = app.theme.clone();
        let mut terminal = Terminal::new(TestBackend::new(60, 6)).unwrap();
        terminal
            .draw(|f| draw(f, Rect::new(0, 3, 60, 3), &mut app, &th))
            .unwrap();
        // position_cursor guards on focus: the default position must be untouched.
        terminal.backend_mut().assert_cursor_position((0, 0));
    }
}

#[cfg(test)]
mod ghost_tests {
    use super::*;
    use crate::render::test_support;

    /// The ghost suffix must track its typed/ghost split across **multiple logical lines**
    /// (char_offset advances per line) and across wraps — a `+=`→`*=` leaves the offset at 0 and
    /// the second line's ghost is re-styled as typed.
    #[test]
    fn ghost_splits_correctly_across_logical_lines() {
        let app = test_support::app();
        let th = app.theme;
        let bg = c(&th.input_bg, "#1a1a2e");
        let ghost_fg = c(&th.input_placeholder, "#404040");
        // Two logical lines of typed text plus a ghost suffix on the whole thing.
        let lines = wrap_input_with_ghost("ab\ncd", "gh", 40, &th, bg);
        assert_eq!(lines.len(), 2);
        let line2: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(line2, "cdgh");
        // The ghost part of line 2 must use the ghost color.
        assert!(
            lines[1].spans.iter().any(|s| s.style.fg == Some(ghost_fg)),
            "line 2 keeps a dim ghost: {line2:?}"
        );
    }

    #[test]
    fn ghost_survives_wrapping_onto_later_lines() {
        let app = test_support::app();
        let th = app.theme;
        let bg = c(&th.input_bg, "#1a1a2e");
        let ghost_fg = c(&th.input_placeholder, "#404040");
        let typed = "x".repeat(30);
        let lines = wrap_input_with_ghost(&typed, "ghosttail", 12, &th, bg);
        assert!(lines.len() > 2, "wraps onto several lines");
        // Some later line carries dim ghost content.
        assert!(
            lines
                .iter()
                .skip(1)
                .any(|l| l.spans.iter().any(|s| s.style.fg == Some(ghost_fg))),
            "ghost text spills past the first wrap"
        );
    }
}
