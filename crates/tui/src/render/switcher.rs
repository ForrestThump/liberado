//! Full-screen unified session switcher (`/sessions`).
//!
//! One list for the whole unified-`Session` model: row 0 is the **primary chat** (the goal-less
//! session); rows 1.. are goal sessions, each labeled by its [`SessionKind`] chip and goal-status
//! so the user can tell at a glance *what kind of agent* each row is.

use chat_client_contract::SessionKind;
use liberado_theme::Theme;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use crate::app::App;
use crate::format::short_id;
use crate::render::kind_color;
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
            "Type to search description / kind…",
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

/// A short `[TAG]` chip for a kind, in that kind's color.
fn kind_chip<'a>(kind: SessionKind, th: &Theme) -> Span<'a> {
    Span::styled(
        format!("[{}]", kind.tag()),
        Style::default()
            .fg(kind_color(kind, th))
            .add_modifier(Modifier::BOLD),
    )
}

fn draw_list(frame: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let border = c(&th.sidebar_border_focused, "#00ffff");
    let text_fg = c(&th.sidebar_text, "#c0c0c0");
    let sel_fg = c(&th.sidebar_selected_fg, "#000000");
    let sel_bg = c(&th.sidebar_selected_bg, "#00ffff");
    let dim = c(&th.chat_system_text, "#808080");
    let accent = c(&th.accent, "#00ffff");
    let bg = c(&th.app_bg, "#0d0d1a");

    let goals = app.filtered_goal_sessions();
    let title = format!(" {} session(s) ", 1 + goals.len());

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(bg));

    let mut items: Vec<ListItem> = Vec::with_capacity(1 + goals.len());

    // Row 0 — the primary chat (goal-less session). Active when not joined to a live session.
    {
        let selected = app.sidebar_selection == 0;
        let active = app.joined.as_ref().map(|j| j.finished).unwrap_or(true);
        let row_style = if selected {
            Style::default().fg(sel_fg).bg(sel_bg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(text_fg).bg(bg)
        };
        let mark = if active { "*" } else { " " };
        let spans = vec![
            Span::styled(format!("{mark} "), row_style),
            kind_chip(SessionKind::Primary, th),
            Span::styled(
                format!("  Primary  ·  {}", SessionKind::Primary.tools_blurb()),
                row_style,
            ),
        ];
        items.push(ListItem::new(Line::from(spans)));
    }

    // Goal rows.
    for (i, h) in goals.iter().enumerate() {
        let selected = app.sidebar_selection == i + 1;
        let joined = app.joined.as_ref().map(|j| j.id == h.id).unwrap_or(false);
        let kind = h.kind();
        let row_style = if selected {
            Style::default().fg(sel_fg).bg(sel_bg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(text_fg).bg(bg)
        };
        let (status_label, status_color) = status_display(h, th);
        let mark = if joined { "*" } else { " " };
        let desc = {
            let d = h.description();
            if d.is_empty() { "(no description)" } else { d }
        };
        let spans = vec![
            Span::styled(format!("{mark} "), row_style),
            kind_chip(kind, th),
            Span::styled(format!("  {:<7} ", kind.label()), row_style),
            Span::styled(
                format!("{status_label:<9} "),
                if selected { row_style } else { Style::default().fg(status_color).bg(bg) },
            ),
            Span::styled(desc.to_string(), row_style),
            Span::styled(
                format!("  [{}]", short_id(&h.id)),
                if selected { row_style } else { Style::default().fg(dim).bg(bg) },
            ),
        ];
        items.push(ListItem::new(Line::from(spans)));
    }

    if goals.is_empty() {
        // Still show the primary row above; add a gentle note beneath it.
        items.push(ListItem::new(Span::styled(
            "  (no goal sessions yet — they appear here once started)",
            Style::default().fg(accent).bg(bg).add_modifier(Modifier::ITALIC),
        )));
    }

    frame.render_widget(List::new(items).block(block), area);
}

/// Status label + color for a goal row: `awaiting` (needs you) stands out; terminal states are
/// colored by outcome.
fn status_display(h: &chat_client_contract::GoalSessionHeader, th: &Theme) -> (String, ratatui::style::Color) {
    if h.awaiting_input && !h.is_terminal() {
        return ("awaiting".into(), c(&th.tool_name, "#ffff00"));
    }
    let color = match h.status.as_str() {
        "running" | "pending" => c(&th.accent, "#00ffff"),
        "succeeded" => c(&th.tool_ok, "#00ff00"),
        "failed" | "cancelled" | "budget_exhausted" => c(&th.tool_err, "#ff0000"),
        _ => c(&th.chat_system_text, "#808080"),
    };
    (h.status.clone(), color)
}

fn draw_hint(frame: &mut Frame, area: Rect, th: &Theme) {
    let dim = c(&th.chat_system_text, "#808080");
    let text = " j/k or ↑↓ move · type to filter · Enter join (row 1 = primary chat) · Esc back ";
    frame.render_widget(
        Paragraph::new(Span::styled(text, Style::default().fg(dim))),
        area,
    );
}
