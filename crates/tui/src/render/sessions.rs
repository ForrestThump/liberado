//! Full-screen searchable session browser (`/session`).

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
            "Type to search titles…",
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

    let visible = app.visible_conversations();
    let title = format!(" {} session(s) ", visible.len());

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(bg));

    if visible.is_empty() {
        let empty = List::new(vec![ListItem::new(Span::styled(
            "  (no conversations yet — chat to create one)",
            Style::default().fg(dim),
        ))])
        .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let items: Vec<ListItem> = visible
        .iter()
        .enumerate()
        .map(|(i, node)| {
            let id_short = short_id(&node.header.id);
            let display = node
                .header
                .title
                .as_deref()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or(id_short);
            let rel = relative_time(&node.header.created_at);
            let active = app.session.as_deref() == Some(node.header.id.as_str());
            let selected = i == app.sidebar_selection;
            let mark = if active { "*" } else { " " };
            let label = format!("{mark} {display}  [{id_short}]  {rel}");

            let style = if selected {
                Style::default()
                    .fg(sel_fg)
                    .bg(sel_bg)
                    .add_modifier(Modifier::BOLD)
            } else if active {
                Style::default().fg(accent).bg(bg)
            } else {
                Style::default().fg(text_fg).bg(bg)
            };
            ListItem::new(Line::from(Span::styled(label, style)))
        })
        .collect();

    frame.render_widget(List::new(items).block(block), area);
}

fn draw_hint(frame: &mut Frame, area: Rect, th: &Theme) {
    let dim = c(&th.chat_system_text, "#808080");
    let text = " j/k or ↑↓ move · type to filter · Enter open · Esc back to chat · n new ";
    frame.render_widget(
        Paragraph::new(Span::styled(text, Style::default().fg(dim))),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ConvHeader;
    use crate::app::Focus;
    use crate::render::test_support;

    fn render(app: &App, w: u16, h: u16) -> String {
        let th = app.theme.clone();
        test_support::render_pane(w, h, |f| draw(f, f.area(), app, &th))
    }

    fn conv(id: &str, title: &str) -> ConvHeader {
        ConvHeader {
            id: id.into(),
            title: Some(title.into()),
            created_at: "2025-06-25T12:00:00Z".into(),
            parent_conversation: None,
            spawned_by: None,
        }
    }

    #[test]
    fn empty_browser_shows_a_message() {
        let mut app = test_support::app();
        app.focus = Focus::SessionBrowser;
        let out = render(&app, 80, 24);
        assert!(out.contains("no conversations yet"), "empty state:\n{out}");
        assert!(out.contains("0 session(s)"), "title count:\n{out}");
        assert!(
            out.contains("Type to search titles"),
            "filter placeholder:\n{out}"
        );
    }

    #[test]
    fn rows_render_titles_short_ids_and_active_mark() {
        let mut app = test_support::app();
        app.focus = Focus::SessionBrowser;
        app.conversations = vec![conv("c1", "weekly planning"), conv("c2", "capture notes")];
        app.sidebar_selection = 1;
        let out = render(&app, 90, 24);
        assert!(out.contains("weekly planning"), "row 1:\n{out}");
        assert!(out.contains("capture notes"), "row 2:\n{out}");
        // The row for the active session carries a star.
        app.session = Some("c1".into());
        let out = render(&app, 90, 24);
        let line = out
            .lines()
            .find(|l| l.contains("weekly planning"))
            .expect("row renders");
        assert!(line.contains('*'), "active marker: {line}");
    }

    #[test]
    fn filter_and_pending_load_render() {
        let mut app = test_support::app();
        app.focus = Focus::SessionBrowser;
        app.conversations = vec![conv("c1", "weekly planning"), conv("c2", "capture notes")];
        app.sidebar_filter = "capture".into();
        app.pending_load = Some("c2".into());
        let out = render(&app, 90, 24);
        assert!(out.contains("capture"), "typed filter text:\n{out}");
        assert!(!out.contains("weekly"), "non-matching hidden:\n{out}");
        assert!(out.contains("1 session(s)"), "filtered count:\n{out}");
    }

    #[test]
    fn parent_child_rows_carry_tree_glyphs() {
        use chat_client_contract::ConvHeader as C;
        let mut app = test_support::app();
        app.focus = Focus::SessionBrowser;
        app.conversations = vec![
            conv("root", "root thread"),
            C {
                id: "kid".into(),
                title: Some("child".into()),
                created_at: "2025-06-25T12:00:00Z".into(),
                parent_conversation: Some("root".into()),
                spawned_by: Some("msg-9".into()),
            },
        ];
        let out = render(&app, 90, 24);
        assert!(out.contains("└──"), "tree glyph:\n{out}");
        assert!(out.contains("child"), "child row:\n{out}");
    }
}
