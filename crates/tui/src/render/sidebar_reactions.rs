//! Reactions panel rendering (recent daemon events with outcome icons).

use liberado_theme::Theme;
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem},
};

use crate::app::App;
use crate::format::truncate_for_display;
use crate::tuning::REACTION_PATH_TRUNCATE;
use crate::ui::c;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Reactions ({}) ", app.reactions.len()))
        .style(Style::default().fg(c(&th.sidebar_text, "#c0c0c0")));

    let items: Vec<ListItem> = app
        .reactions
        .iter()
        .rev()
        .take(area.height.saturating_sub(2) as usize)
        .map(|r| {
            let icon = match r.outcome.as_str() {
                "observed" => Span::styled(
                    "◉",
                    Style::default().fg(c(&th.reaction_observed, "#00ffff")),
                ),
                "dispatched" => Span::styled(
                    "→",
                    Style::default().fg(c(&th.reaction_dispatched, "#ffff00")),
                ),
                "acted" => Span::styled(
                    "✓",
                    Style::default().fg(c(&th.reaction_acted, "#00ff00")),
                ),
                "reported" => Span::styled(
                    "✓",
                    Style::default().fg(c(&th.reaction_acted, "#00ff00")),
                ),
                _ => Span::styled(
                    "?",
                    Style::default().fg(c(&th.reaction_unknown, "#808080")),
                ),
            };
            let path = r.path.as_deref().unwrap_or("?").to_string();
            let label = format!(
                " {}  {}",
                r.event_type,
                truncate_for_display(&path, REACTION_PATH_TRUNCATE)
            );
            ListItem::new(Line::from(vec![icon, Span::raw(label)]))
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}
