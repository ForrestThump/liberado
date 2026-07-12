//! Full-screen searchable session browser (`/session`).

use liberado_theme::Theme;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use crate::app::App;
use crate::format::{relative_time, short_id};
use crate::ui::c;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // filter
            Constraint::Min(1),    // list
            Constraint::Length(1), // hint
        ])
        .split(area);

    draw_filter(frame, chunks[0], app, th);
    draw_list(frame, chunks[1], app, th);
    draw_hint(frame, chunks[2], th);
}

fn draw_filter(frame: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let border = c(&th.input_border_focused, "#00ffff");
    let fg = c(&th.input_text, "#ffffff");
    let bg = c(&th.input_bg, "#1a1a2e");
    let placeholder = c(&th.input_placeholder, "#404040");

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Sessions — filter ")
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(bg));

    let text = if app.sidebar_filter.is_empty() {
        Line::from(Span::styled(
            "Type to search titles…",
            Style::default().fg(placeholder).bg(bg),
        ))
    } else {
        Line::from(Span::styled(
            app.sidebar_filter.clone(),
            Style::default().fg(fg).bg(bg),
        ))
    };

    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn draw_list(frame: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let border = c(&th.sidebar_border_focused, "#00ffff");
    let text_fg = c(&th.sidebar_text, "#c0c0c0");
    let sel_fg = c(&th.sidebar_selected_fg, "#000000");
    let sel_bg = c(&th.sidebar_selected_bg, "#00ffff");
    let accent = c(&th.accent, "#00ffff");
    let dim = c(&th.chat_system_text, "#808080");
    let bg = c(&th.app_bg, "#0d0d1a");

    let visible = app.visible_conversations();
    let title = format!(" {} session(s) ", visible.len());

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(bg));

    if visible.is_empty() {
        let empty = List::new(vec![ListItem::new(Span::styled(
            "  (no conversations yet — chat to create one)",
            Style::default().fg(dim),
        ))])
        .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let items: Vec<ListItem> = visible
        .iter()
        .enumerate()
        .map(|(i, node)| {
            let id_short = short_id(&node.header.id);
            let display = node
                .header
                .title
                .as_deref()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or(id_short);
            let rel = relative_time(&node.header.created_at);
            let active = app.session.as_deref() == Some(node.header.id.as_str());
            let selected = i == app.sidebar_selection;
            let mark = if active { "*" } else { " " };
            let label = format!("{mark} {display}  [{id_short}]  {rel}");

            let style = if selected {
                Style::default().fg(sel_fg).bg(sel_bg).add_modifier(Modifier::BOLD)
            } else if active {
                Style::default().fg(accent).bg(bg)
            } else {
                Style::default().fg(text_fg).bg(bg)
            };
            ListItem::new(Line::from(Span::styled(label, style)))
        })
        .collect();

    frame.render_widget(List::new(items).block(block), area);
}

fn draw_hint(frame: &mut Frame, area: Rect, th: &Theme) {
    let dim = c(&th.chat_system_text, "#808080");
    let text = " j/k or ↑↓ move · type to filter · Enter open · Esc back to chat · n new ";
    frame.render_widget(
        Paragraph::new(Span::styled(text, Style::default().fg(dim))),
        area,
    );
}
