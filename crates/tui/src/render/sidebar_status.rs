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
use crate::format::{safe_truncate, truncate_path};
use crate::tuning::VAULT_PATH_TRUNCATE;
use crate::ui::c;
use liberado_commands::format_uptime;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::DaemonStatus;
    use crate::render::test_support;

    fn render(app: &App, w: u16, h: u16) -> String {
        let th = app.theme.clone();
        test_support::render_pane(w, h, |f| draw(f, f.area(), app, &th))
    }

    fn status(running: bool) -> DaemonStatus {
        DaemonStatus {
            running,
            vault_path: "/home/user/vault".into(),
            uptime_seconds: 3725,
            watcher_active: false,
            dispatcher_attached: true,
            orchestrator_attached: true,
            reactions_seen: 0,
            model_name: None,
            token_usage_total: None,
            context_window: None,
            chat_tools: 3,
            chat_tool_names: vec!["a".into(), "b".into(), "c".into()],
            enter_sends: true,
        }
    }

    #[test]
    fn running_daemon_renders_health_and_attachments() {
        let mut app = test_support::app();
        app.status = Some(status(true));
        let out = render(&app, 40, 12);
        assert!(out.contains("Daemon: running"), "running label:\n{out}");
        assert!(out.contains('●'), "online dot:\n{out}");
        assert!(out.contains("Uptime"), "uptime:\n{out}");
        assert!(out.contains("/home/user/vault"), "vault path:\n{out}");
        assert!(out.contains("Dispatcher ✓"), "dispatcher:\n{out}");
        assert!(out.contains("Orchestrator ✓"), "orchestrator:\n{out}");
        assert!(out.contains("Chat tools: 3"), "tool count:\n{out}");
    }

    #[test]
    fn stopped_daemon_and_no_tools_render() {
        let mut app = test_support::app();
        let mut s = status(false);
        s.chat_tools = 0;
        s.chat_tool_names.clear();
        s.dispatcher_attached = false;
        s.orchestrator_attached = false;
        app.status = Some(s);
        let out = render(&app, 40, 12);
        assert!(out.contains("Daemon: stopped"), "stopped label:\n{out}");
        assert!(out.contains('○'), "offline dot:\n{out}");
        assert!(
            out.contains("Chat tools: 0 (conversation-only)"),
            "zero-tools note:\n{out}"
        );
        assert!(!out.contains("Dispatcher"), "no dispatcher line:\n{out}");
    }

    #[test]
    fn without_status_the_server_line_and_connecting_dot_show() {
        let mut app = test_support::app();
        app.server = "http://127.0.0.1:4201".into();
        let out = render(&app, 40, 12);
        assert!(out.contains("Server: http://"), "server line:\n{out}");
        assert!(out.contains("connecting"), "connecting hint:\n{out}");
    }

    #[test]
    fn long_server_urls_are_truncated() {
        let mut app = test_support::app();
        app.server = "http://127.0.0.1:4201/very/long/path/that/overflows".into();
        let out = render(&app, 40, 12);
        assert!(out.contains("..."), "truncation marker:\n{out}");
        assert!(!out.contains("overflows"), "tail gone:\n{out}");
    }
}
