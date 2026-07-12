//! Full-screen searchable model browser (`/model`).

use liberado_theme::Theme;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use crate::app::App;
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
        .title(" Models — filter ")
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(bg));

    let text = if app.sidebar_filter.is_empty() {
        Line::from(Span::styled(
            "Type to search model ids…",
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

    let filtered = app.filtered_models();
    let current = app.status.as_ref().and_then(|s| s.model_name.as_deref());
    let title = if app.models_loading {
        " loading models… ".to_string()
    } else {
        format!(" {} model(s) ", filtered.len())
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(bg));

    if app.models_loading && app.models.is_empty() {
        let empty = List::new(vec![ListItem::new(Span::styled(
            "  Fetching model catalog from daemon…",
            Style::default().fg(dim),
        ))])
        .block(block);
        frame.render_widget(empty, area);
        return;
    }

    if filtered.is_empty() {
        let msg = if let Some(err) = &app.models_error {
            format!("  (no models — {err})")
        } else if app.models.is_empty() {
            "  (provider returned no models)".to_string()
        } else {
            "  (no matches for filter)".to_string()
        };
        let empty = List::new(vec![ListItem::new(Span::styled(
            msg,
            Style::default().fg(dim),
        ))])
        .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let items: Vec<ListItem> = filtered
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let is_current = current == Some(name.as_str());
            let selected = i == app.sidebar_selection;
            let mark = if is_current { "*" } else { " " };
            let label = format!(" {mark} {name}");
            let style = if selected {
                Style::default()
                    .fg(sel_fg)
                    .bg(sel_bg)
                    .add_modifier(Modifier::BOLD)
            } else if is_current {
                Style::default().fg(accent)
            } else {
                Style::default().fg(text_fg)
            };
            ListItem::new(Span::styled(label, style))
        })
        .collect();

    frame.render_widget(List::new(items).block(block), area);
}

fn draw_hint(frame: &mut Frame, area: Rect, th: &Theme) {
    let dim = c(&th.chat_system_text, "#808080");
    let text = " ↑/↓ j/k navigate · type to filter · Enter switch model · r refresh · Esc close ";
    frame.render_widget(
        Paragraph::new(Span::styled(text, Style::default().fg(dim))),
        area,
    );
}
