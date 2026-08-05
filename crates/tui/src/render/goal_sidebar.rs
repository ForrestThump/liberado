//! Goal session sidebar — live gate votes, active role, and last validation result.
//!
//! Rendered to the right of the chat pane when a goal session is joined.
//! Gate votes update in real time as each reviewer's verdict streams in.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
};

use crate::app::App;
use crate::ui::c;

pub fn draw(frame: &mut Frame, area: Rect, app: &App, th: &liberado_theme::Theme) {
    let Some(j) = app.joined.as_ref() else {
        return;
    };
    if area.width == 0 || area.height == 0 {
        return;
    }

    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(Span::styled(
        " Gate Votes ",
        Style::default()
            .fg(c(&th.accent, "#00ffff"))
            .add_modifier(Modifier::BOLD),
    )));

    if j.gate_votes.is_empty() {
        lines.push(Line::from(Span::styled(
            " (no votes yet)",
            Style::default()
                .fg(c(&th.chat_system_text, "#808080"))
                .add_modifier(Modifier::ITALIC),
        )));
    } else {
        for vote in &j.gate_votes {
            let (mark, style) = if vote.coerced {
                (
                    "?",
                    Style::default()
                        .fg(c(&th.md_bullet, "#ffff00"))
                        .add_modifier(Modifier::BOLD),
                )
            } else if vote.approved {
                (
                    "✓",
                    Style::default()
                        .fg(c(&th.tool_ok, "#00ff00"))
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                (
                    "✗",
                    Style::default()
                        .fg(c(&th.tool_err, "#ff0000"))
                        .add_modifier(Modifier::BOLD),
                )
            };
            let kind_short = match vote.kind.as_str() {
                "gatekeeper" => "G",
                "fresh" => "F",
                "strategist" => "S",
                other => other,
            };
            lines.push(Line::from(vec![
                Span::styled(mark, style),
                Span::raw(" "),
                Span::styled(
                    kind_short,
                    Style::default()
                        .fg(c(&th.chat_system_text, "#808080"))
                        .add_modifier(Modifier::ITALIC),
                ),
                Span::raw(" "),
                Span::styled(
                    &vote.reviewer,
                    Style::default().fg(c(&th.sidebar_text, "#c0c0c0")),
                ),
            ]));
        }
    }

    lines.push(Line::from(""));

    if let Some(role) = &j.active_role {
        lines.push(Line::from(Span::styled(
            " Active Role ",
            Style::default()
                .fg(c(&th.accent, "#00ffff"))
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!(" ▶ {role}"),
            Style::default().fg(c(&th.tool_ok, "#00ff00")),
        )));
    } else if j.finished {
        lines.push(Line::from(Span::styled(
            " Status ",
            Style::default()
                .fg(c(&th.accent, "#00ffff"))
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!(" ● {status}", status = j.status),
            Style::default()
                .fg(c(&th.chat_system_text, "#808080"))
                .add_modifier(Modifier::ITALIC),
        )));
    }

    lines.push(Line::from(""));

    lines.push(Line::from(Span::styled(
        " Validation ",
        Style::default()
            .fg(c(&th.accent, "#00ffff"))
            .add_modifier(Modifier::BOLD),
    )));
    if let Some(v) = &j.last_validation {
        let (mark, style) = if v.ok {
            (
                "✓",
                Style::default()
                    .fg(c(&th.tool_ok, "#00ff00"))
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            (
                "✗",
                Style::default()
                    .fg(c(&th.tool_err, "#ff0000"))
                    .add_modifier(Modifier::BOLD),
            )
        };
        lines.push(Line::from(vec![
            Span::styled(mark, style),
            Span::raw(" "),
            Span::styled(
                &v.summary,
                Style::default().fg(c(&th.sidebar_text, "#c0c0c0")),
            ),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            " (not yet)",
            Style::default()
                .fg(c(&th.chat_system_text, "#808080"))
                .add_modifier(Modifier::ITALIC),
        )));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(c(&th.sidebar_border_focused, "#404060")))
        .title_top(Line::from(Span::styled(
            format!(" {} ", j.description.chars().take(20).collect::<String>()),
            Style::default()
                .fg(c(&th.accent, "#00ffff"))
                .add_modifier(Modifier::BOLD),
        )));

    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, area);
}
