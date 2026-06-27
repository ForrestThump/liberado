//! Status panel rendering (daemon health, uptime, vault path).

use liberado_theme::Theme;
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::App;
use crate::format::{format_uptime, safe_truncate, truncate_path};
use crate::tuning::VAULT_PATH_TRUNCATE;
use crate::ui::c;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Status ")
        .style(Style::default().fg(c(&th.sidebar_text, "#c0c0c0")));

    let mut lines: Vec<Line> = Vec::new();

    if let Some(ref status) = app.status {
        let (dot, label) = if status.running {
            ("●", "running")
        } else {
            ("○", "stopped")
        };
        let dot_color = if status.running {
            c(&th.status_dot_online, "#00ff00")
        } else {
            c(&th.status_dot_offline, "#ff0000")
        };
        lines.push(Line::from(vec![
            Span::styled(dot, Style::default().fg(dot_color)),
            Span::raw(format!(" Daemon: {label}")),
        ]));

        let uptime = format_uptime(status.uptime_seconds);
        lines.push(Line::from(format!("  Uptime: {uptime}")));
        lines.push(Line::from(format!(
            "  Vault: {}",
            truncate_path(&status.vault_path, VAULT_PATH_TRUNCATE)
        )));

        if status.dispatcher_attached {
            lines.push(Line::from(Span::styled(
                "  Dispatcher ✓",
                Style::default().fg(c(&th.status_dot_online, "#00ff00")),
            )));
        }
        if status.orchestrator_attached {
            lines.push(Line::from(Span::styled(
                "  Orchestrator ✓",
                Style::default().fg(c(&th.status_dot_online, "#00ff00")),
            )));
        }

        let tool_count = status.chat_tools;
        if tool_count > 0 {
            lines.push(Line::from(format!("  Chat tools: {tool_count}")));
        } else {
            lines.push(Line::from(Span::styled(
                "  Chat tools: 0 (conversation-only)",
                Style::default().fg(c(&th.status_dot_connecting, "#ffff00")),
            )));
        }
    } else {
        let s = &app.server;
        let short = if s.len() > 20 {
            format!("{}...", safe_truncate(s, 17))
        } else {
            s.clone()
        };
        lines.push(Line::from(format!("  Server: {short}")));
        lines.push(Line::from(Span::styled(
            "  ● connecting…",
            Style::default().fg(c(&th.status_dot_connecting, "#ffff00")),
        )));
    }

    let paragraph = Paragraph::new(Text::from(lines)).block(block);
    frame.render_widget(paragraph, area);
}
