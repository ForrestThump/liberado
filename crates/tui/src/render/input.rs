use liberado_theme::Theme;
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Span, Text},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{App, Focus};
use crate::ui::c;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let border_color = if app.focus == Focus::Input {
        c(&th.input_border_focused, "#00ffff")
    } else {
        c(&th.input_border_unfocused, "#404040")
    };
    let border_style = Style::default().fg(border_color);

    let streaming_indicator = if app.streaming { " [streaming…]" } else { "" };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " Message{streaming_indicator} | Enter to send, Shift+Enter for newline, Esc to clear | Ctrl+C to quit "
        ))
        .border_style(border_style);

    let text = if app.input.is_empty() && app.focus == Focus::Input {
        Text::from(Span::styled(
            "Type a message...",
            Style::default().fg(c(&th.input_placeholder, "#404040")),
        ))
    } else {
        Text::from(Span::styled(
            app.input.clone(),
            Style::default().fg(c(&th.input_text, "#ffffff")),
        ))
    };

    let paragraph = Paragraph::new(text).block(block);

    frame.render_widget(paragraph, area);

    if app.focus == Focus::Input {
        let cursor_x = (area.x + 1 + app.cursor as u16).min(area.right().saturating_sub(1));
        frame.set_cursor_position((cursor_x, area.y + 1));
    }
}
