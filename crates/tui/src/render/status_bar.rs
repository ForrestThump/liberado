use liberado_theme::Theme;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::App;
use crate::format::short_id;
use crate::render::kind_color;
use crate::tuning::*;
use crate::ui::c;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, spinner_tick: u8, th: &Theme) {
    let summary = app.status_summary();

    // At-a-glance session identity: a colored kind chip. When joined to a goal session it shows
    // that session's kind + short id + live status; otherwise the primary chat.
    let kind = app.current_kind();
    let chip = Span::styled(
        format!(" {} ", kind.tag()),
        Style::default()
            .fg(c(&th.app_bg, "#0d0d1a"))
            .bg(kind_color(kind, th))
            .add_modifier(Modifier::BOLD),
    );
    let kind_label = match app.joined.as_ref() {
        Some(j) if !j.finished => format!(" {} {} · {} ", kind.label(), short_id(&j.id), j.status),
        _ => format!(" {} ", kind.label()),
    };
    let kind_label_span = Span::styled(
        kind_label,
        Style::default().fg(kind_color(kind, th)).add_modifier(Modifier::BOLD),
    );

    let spinner = if summary.streaming {
        SPINNER_FRAMES[(spinner_tick as usize) % SPINNER_FRAMES.len()]
    } else {
        ' '
    };

    let dot = if summary.connected { "●" } else { "○" };
    let dot_style = if summary.connected {
        Style::default().fg(c(&th.status_dot_online, "#00ff00"))
    } else {
        Style::default().fg(c(&th.status_dot_offline, "#ff0000"))
    };

    let session_str = summary
        .session_id
        .as_deref()
        .map(|id| format!("session: {}", short_id(id)))
        .unwrap_or_else(|| "new conversation".into());

    let uptime_str = summary
        .uptime
        .as_deref()
        .map(|u| format!("up: {u}"))
        .unwrap_or_else(|| "connecting...".into());

    let mut bar_text = format!(
        " Liberado  {uptime_str}  |  {session_str}  |  {} messages",
        summary.message_count
    );

    if let Some(ref model) = summary.model_name {
        bar_text.push_str(&format!("  |  {model}"));
        if let (Some(used), Some(window)) = (summary.token_usage_total, summary.context_window)
            && window > 0
        {
            let pct = (used as f64 / window as f64 * 100.0).min(CTX_PCT_DISPLAY_CAP) as u32;
            bar_text.push_str(&format!("  [{pct}% ctx]"));
        }
    }

    if !summary.connected {
        let spinner = SPINNER_FRAMES[(spinner_tick as usize) % SPINNER_FRAMES.len()];
        bar_text = format!(" Liberado  [{spinner} reconnecting…]  |  {session_str}");
    }

    let bar_color = c(&th.status_bar_text, "#808080");

    let mut spans = vec![
        Span::styled(dot, dot_style),
        chip,
        kind_label_span,
        Span::styled(bar_text, Style::default().fg(bar_color)),
    ];

    if summary.streaming {
        spans.push(Span::styled(
            format!("  [{spinner} streaming]"),
            Style::default().fg(c(&th.accent, "#00ffff")),
        ));
    }

    let paragraph = Paragraph::new(Line::from(spans));
    frame.render_widget(paragraph, area);
}
