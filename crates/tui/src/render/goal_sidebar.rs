//! Goal session sidebar — live gate votes, active role, and last validation result.
//!
//! Rendered to the right of the chat pane when a goal session is joined.
//! Gate votes update in real time as each reviewer's verdict streams in.
//!
//! A `Paragraph` clips, it does not scroll, so the pane's height is a budget that has to be
//! divided rather than filled top-down. The vote list is the only unbounded section, so it is
//! built *last* against whatever the fixed sections left over: fill top-down instead and a long
//! gate run pushes "Active Role" and "Validation" off the bottom, and — because the list renders
//! oldest-first — leaves the oldest votes on screen while the ones still streaming in fall off.
//! That is the opposite of what the pane is for; the chat transcript already holds the full
//! history and scrolls.

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

    let heading = |text: &'static str| {
        Line::from(Span::styled(
            text,
            Style::default()
                .fg(c(&th.accent, "#00ffff"))
                .add_modifier(Modifier::BOLD),
        ))
    };
    let muted = |text: String| {
        Line::from(Span::styled(
            text,
            Style::default()
                .fg(c(&th.chat_system_text, "#808080"))
                .add_modifier(Modifier::ITALIC),
        ))
    };

    // ── The fixed sections, built first because they claim their space first ──────────────
    let mut tail: Vec<Line<'_>> = Vec::new();

    if let Some(role) = &j.active_role {
        tail.push(Line::from(""));
        tail.push(heading(" Active Role "));
        tail.push(Line::from(Span::styled(
            format!(" ▶ {role}"),
            Style::default().fg(c(&th.tool_ok, "#00ff00")),
        )));
    } else if j.finished {
        tail.push(Line::from(""));
        tail.push(heading(" Status "));
        tail.push(muted(format!(" ● {status}", status = j.status)));
    }

    tail.push(Line::from(""));
    tail.push(heading(" Validation "));
    match &j.last_validation {
        Some(v) => {
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
            tail.push(Line::from(vec![
                Span::styled(mark, style),
                Span::raw(" "),
                Span::styled(
                    &v.summary,
                    Style::default().fg(c(&th.sidebar_text, "#c0c0c0")),
                ),
            ]));
        }
        None => tail.push(muted(" (not yet)".to_string())),
    }

    // ── The vote list, against what is left ──────────────────────────────────────────────
    let inner_height = area.height.saturating_sub(2) as usize; // top and bottom border
    // One line for the " Gate Votes " heading; the rest of the budget is rows.
    let budget = inner_height.saturating_sub(tail.len() + 1);

    let mut lines: Vec<Line<'_>> = vec![heading(" Gate Votes ")];

    if j.gate_votes.is_empty() {
        if budget > 0 {
            lines.push(muted(" (no votes yet)".to_string()));
        }
    } else if budget > 0 {
        // Show the newest votes. When some do not fit, one row of the budget goes to saying how
        // many were dropped — silently showing a subset would read as "that is all of them".
        let shown = if j.gate_votes.len() > budget {
            budget - 1
        } else {
            budget
        };
        let hidden = j.gate_votes.len().saturating_sub(shown);
        if hidden > 0 {
            lines.push(muted(format!(" … {hidden} earlier")));
        }
        for vote in j.gate_votes.iter().skip(hidden) {
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

    lines.extend(tail);

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
