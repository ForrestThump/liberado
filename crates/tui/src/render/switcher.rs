//! Full-screen unified session switcher (`/session` and `/sessions`).
//!
//! One flat list for the whole unified-`Session` model: prior conversations (primary chats) first,
//! then goal sessions. Every row carries its [`SessionKind`] chip so the user can tell at a glance
//! *what kind of agent* each row is; goal rows also show their status.

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
use crate::format::{relative_time, short_id};
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
            "Type to search chats & sessions…",
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

    let sessions = app.filtered_sessions();
    let title = format!(" {} session(s) ", sessions.len());

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(bg));

    let mut items: Vec<ListItem> = Vec::with_capacity(sessions.len());

    // Not focused on a live goal session → the active conversation is the current surface.
    let on_primary = app.joined.as_ref().map(|j| j.finished).unwrap_or(true);

    // ONE loop over ONE list (S5′). A row's kind, its status column, and whether Enter joins or
    // opens it all fall out of the same `goal: Option` the store uses — the client no longer
    // maintains a parallel notion of "chat rows" versus "goal rows".
    for (i, h) in sessions.iter().enumerate() {
        let selected = app.sidebar_selection == i;
        let kind = h.kind();
        let joined = app.joined.as_ref().map(|j| j.id == h.id).unwrap_or(false);
        let active = if h.has_goal() {
            joined
        } else {
            on_primary && app.session.as_deref() == Some(h.id.as_str())
        };
        let row_style = if selected {
            Style::default()
                .fg(sel_fg)
                .bg(sel_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(text_fg).bg(bg)
        };
        let dim_style = if selected {
            row_style
        } else {
            Style::default().fg(dim).bg(bg)
        };
        let mark = if active { "*" } else { " " };

        // Nobody started this one — a cron, a webhook, a delegated subagent (S5′ step 5). Without a
        // marker these rows read as things *you* launched and forgot, which is the opposite of the
        // truth. Fixed-width so the columns stay aligned whether or not the tag is there.
        let unattended = h.visibility == chat_client_contract::VisibilityWire::Background;
        let bg_tag = if unattended { "bg " } else { "   " };

        let mut spans = vec![
            Span::styled(format!("{mark} "), row_style),
            kind_chip(kind, th),
            Span::styled(format!("  {:<7} ", kind.label()), row_style),
            Span::styled(bg_tag, dim_style),
        ];
        // A chat has no lifecycle to report — it is simply open — so the status column belongs to
        // goal-bearing rows only. Blanking it keeps the columns aligned.
        if h.has_goal() {
            let (status_label, status_color) = status_display(h, th);
            spans.push(Span::styled(
                format!("{status_label:<9} "),
                if selected {
                    row_style
                } else {
                    Style::default().fg(status_color).bg(bg)
                },
            ));
        } else {
            spans.push(Span::styled(format!("{:<9} ", ""), row_style));
        }

        let label = {
            let l = h.label();
            if l.is_empty() { "(untitled)" } else { l }
        };
        spans.push(Span::styled(label.to_string(), row_style));
        spans.push(Span::styled(
            format!("  {}", relative_time(&h.created_at)),
            dim_style,
        ));
        spans.push(Span::styled(format!("  [{}]", short_id(&h.id)), dim_style));

        items.push(ListItem::new(Line::from(spans)));
    }

    if items.is_empty() {
        items.push(ListItem::new(Span::styled(
            "  (no sessions yet — send a message to start one, or /spawn a goal)",
            Style::default()
                .fg(accent)
                .bg(bg)
                .add_modifier(Modifier::ITALIC),
        )));
    }

    frame.render_widget(List::new(items).block(block), area);
}

/// Status label + color for a goal row: `awaiting` (needs you) stands out; terminal states are
/// colored by outcome.
fn status_display(
    h: &chat_client_contract::SessionSummary,
    th: &Theme,
) -> (String, ratatui::style::Color) {
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
    let text = " j/k or ↑↓ move · type to filter · Enter open chat / join session · Esc back ";
    frame.render_widget(
        Paragraph::new(Span::styled(text, Style::default().fg(dim))),
        area,
    );
}
