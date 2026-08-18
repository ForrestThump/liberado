//! Reactions panel rendering (recent daemon events with outcome icons).

use chat_client_contract::ReactionOutcome;
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
            let icon = match &r.outcome {
                ReactionOutcome::Observed => Span::styled(
                    "◉",
                    Style::default().fg(c(&th.reaction_observed, "#00ffff")),
                ),
                ReactionOutcome::Decided => Span::styled(
                    "→",
                    Style::default().fg(c(&th.reaction_dispatched, "#ffff00")),
                ),
                ReactionOutcome::Acted => {
                    Span::styled("✓", Style::default().fg(c(&th.reaction_acted, "#00ff00")))
                }
                ReactionOutcome::Dispatched { .. } => {
                    Span::styled("▶", Style::default().fg(c(&th.reaction_acted, "#00ff00")))
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ReactionEvent;
    use crate::render::test_support;

    fn render(app: &App, w: u16, h: u16) -> String {
        let th = app.theme.clone();
        test_support::render_pane(w, h, |f| draw(f, f.area(), app, &th))
    }

    fn event(event_type: &str, outcome: ReactionOutcome, path: Option<&str>) -> ReactionEvent {
        ReactionEvent {
            event_type: event_type.into(),
            timestamp: "2025-06-25T12:00:00Z".into(),
            source: "watcher".into(),
            correlation_id: "x".into(),
            path: path.map(str::to_string),
            outcome,
        }
    }

    #[test]
    fn empty_reactions_show_a_zero_count() {
        let app = test_support::app();
        let out = render(&app, 40, 12);
        assert!(out.contains("Reactions (0)"), "title:\n{out}");
    }

    #[test]
    fn every_outcome_gets_its_icon_and_path() {
        use chat_client_contract::ReactionOutcome as O;
        let mut app = test_support::app();
        app.reactions = vec![
            event("file_changed", O::Observed, Some("/docs/notes.md")),
            event("task_decided", O::Decided, Some("/docs/x.md")),
            event("task_acted", O::Acted, None),
            event(
                "dispatched",
                O::Dispatched {
                    session_id: "s1".into(),
                },
                Some("/docs/y.md"),
            ),
        ];
        let out = render(&app, 50, 12);
        assert!(out.contains("◉"), "observed icon:\n{out}");
        assert!(out.contains('→'), "decided icon:\n{out}");
        assert!(out.contains('✓'), "acted icon:\n{out}");
        assert!(out.contains('▶'), "dispatched icon:\n{out}");
        assert!(out.contains("file_changed"), "event type:\n{out}");
        assert!(out.contains("/docs/notes.md"), "path:\n{out}");
        assert!(out.contains('?'), "missing path fallback:\n{out}");
    }
}
