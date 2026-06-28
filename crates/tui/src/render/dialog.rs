use liberado_theme::Theme;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};

use crate::app::App;
use crate::tuning::{DIALOG_MAX_HEIGHT, DIALOG_MIN_HEIGHT, DIALOG_WIDTH};
use crate::ui::c;

pub(super) fn draw(frame: &mut Frame, app: &App, th: &Theme) {
    let Some(ref dialog) = app.dialog else {
        return;
    };

    let text_color = c(&th.chat_system_text, "#9ca3af");
    let highlight_bg = c(&th.sidebar_selected_bg, "#374151");
    let highlight_fg = c(&th.sidebar_selected_fg, "#ffffff");
    let dim_color = c(&th.chat_system_text, "#6b7280");

    let mut all_lines: Vec<Line> = Vec::new();
    let option_start = dialog.lines.len();

    for line in &dialog.lines {
        for sub in line.split('\n') {
            all_lines.push(Line::from(Span::styled(sub, Style::default().fg(text_color))));
        }
    }

    for (i, (label, _value)) in dialog.options.iter().enumerate() {
        let cursor_idx = option_start + i;
        let style = if cursor_idx == dialog.cursor {
            Style::default().fg(highlight_fg).bg(highlight_bg)
        } else {
            Style::default().fg(text_color)
        };
        all_lines.push(Line::from(Span::styled(format!("  {label}"), style)));
    }

    let total_content = all_lines.len();
    let dialog_height = if total_content == 0 {
        DIALOG_MIN_HEIGHT
    } else {
        ((total_content as u16) + 3).clamp(DIALOG_MIN_HEIGHT, DIALOG_MAX_HEIGHT)
    }
    .min(frame.area().height.saturating_sub(4));
    let dialog_width = DIALOG_WIDTH.min(frame.area().width.saturating_sub(8));

    let area = centered_rect(frame.area(), dialog_width, dialog_height);

    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(c(&th.sidebar_border_focused, "#6b7280")))
        .title(Span::styled(&dialog.title, Style::default().fg(c(&th.accent, "#e5e7eb"))))
        .style(Style::default().bg(c(&th.app_bg, "#0d0d1a")));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let visible = inner.height as usize;
    if visible == 0 || total_content == 0 {
        return;
    }

    let mut scroll = dialog.cursor;
    if scroll + visible > total_content {
        scroll = total_content.saturating_sub(visible);
    }
    if scroll > dialog.cursor {
        scroll = dialog.cursor;
    }

    let mut display_lines = Vec::new();

    if scroll > 0 {
        display_lines.push(Line::from(Span::styled(
            "\u{2191} more above",
            Style::default().fg(dim_color).add_modifier(Modifier::DIM),
        )));
    }

    for (_i, line) in all_lines.iter().enumerate().skip(scroll).take(visible) {
        display_lines.push(line.clone());
    }

    if scroll + visible < total_content {
        display_lines.push(Line::from(Span::styled(
            "\u{2193} more below",
            Style::default().fg(dim_color).add_modifier(Modifier::DIM),
        )));
    }

    let paragraph = Paragraph::new(display_lines).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((area.height.saturating_sub(height)) / 2),
            Constraint::Length(height),
            Constraint::Length((area.height.saturating_sub(height)) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((area.width.saturating_sub(width)) / 2),
            Constraint::Length(width),
            Constraint::Length((area.width.saturating_sub(width)) / 2),
        ])
        .split(popup[1])[1]
}
