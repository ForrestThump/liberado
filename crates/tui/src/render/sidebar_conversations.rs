//! Conversations panel rendering (tree-list of chat sessions).

use liberado_theme::Theme;
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::Span,
    widgets::{Block, Borders, List, ListItem},
};

use crate::app::App;
use crate::format::{relative_time, short_id};
use crate::ui::c;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let border_color = if app.focus == crate::app::Focus::SessionBrowser {
        c(&th.sidebar_border_focused, "#00ffff")
    } else {
        c(&th.sidebar_border_unfocused, "#808080")
    };

    let title = if app.sidebar_filter.is_empty() {
        " Conversations (n=new, Space=fold, Enter=open) ".into()
    } else {
        format!(" Conversations (filter: \"{}\") ", app.sidebar_filter)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(border_color));

    let visible = app.visible_conversations();
    let items: Vec<ListItem> = visible
        .iter()
        .enumerate()
        .map(|(i, node)| {
            // Prefer persisted title (first-line default, agent, or /title). Fall back to short id.
            let id_short = short_id(&node.header.id);
            let display = node
                .header
                .title
                .as_deref()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or(id_short);
            let rel = relative_time(&node.header.created_at);
            let active = app.session.as_deref() == Some(node.header.id.as_str());
            let is_selected =
                i == app.sidebar_selection && app.focus == crate::app::Focus::SessionBrowser;
            let is_pending = app.pending_load.as_deref() == Some(node.header.id.as_str());

            let (fg, bg) = if is_selected {
                (
                    c(&th.sidebar_selected_fg, "#000000"),
                    c(&th.sidebar_selected_bg, "#00ffff"),
                )
            } else if active {
                (c(&th.accent, "#00ffff"), c(&th.sidebar_item_bg, "#000000"))
            } else {
                (
                    c(&th.sidebar_text, "#c0c0c0"),
                    c(&th.sidebar_item_bg, "#000000"),
                )
            };

            let mut prefix = String::new();
            for d in 0..node.depth {
                let ancestor_last = *node.ancestors_last.get(d).unwrap_or(&false);
                if ancestor_last {
                    prefix.push_str("    ");
                } else {
                    prefix.push_str("│   ");
                }
            }
            if node.depth > 0 {
                if node.is_last {
                    prefix.push_str("└── ");
                } else {
                    prefix.push_str("├── ");
                }
            }
            let fold_icon = if node.has_children {
                if node.collapsed { "▶ " } else { "▼ " }
            } else if node.depth > 0 {
                "  "
            } else {
                ""
            };
            let active_mark = if active { "*" } else { " " };
            let loading = if is_pending { " …" } else { "" };
            let text =
                format!("{prefix}{fold_icon}{active_mark} {display}{loading}  [{id_short}]  {rel}");
            ListItem::new(text).style(Style::default().fg(fg).bg(bg))
        })
        .collect();

    if items.is_empty() {
        let empty = vec![ListItem::new(Span::styled(
            "  (no conversations)",
            Style::default().fg(c(&th.chat_system_text, "#808080")),
        ))];
        let list = List::new(empty).block(block);
        frame.render_widget(list, area);
    } else {
        let list = List::new(items).block(block);
        frame.render_widget(list, area);
    }
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
    fn empty_list_shows_the_nothing_message() {
        let mut app = test_support::app();
        app.focus = Focus::SessionBrowser;
        let out = render(&app, 40, 12);
        assert!(out.contains("no conversations"), "empty:\n{out}");
        assert!(out.contains("Conversations"), "title:\n{out}");
    }

    #[test]
    fn rows_show_title_short_id_and_relative_time() {
        let mut app = test_support::app();
        app.focus = Focus::SessionBrowser;
        app.conversations = vec![conv("c1", "weekly planning"), conv("c2", "capture notes")];
        let out = render(&app, 60, 12);
        assert!(out.contains("weekly planning"), "row:\n{out}");
        assert!(out.contains("[c1]"), "short id:\n{out}");
        assert!(out.contains("Jun 25"), "relative time:\n{out}");
    }

    #[test]
    fn selected_active_and_loading_states_mark_rows() {
        let mut app = test_support::app();
        app.focus = Focus::SessionBrowser;
        app.conversations = vec![conv("c1", "weekly planning"), conv("c2", "capture notes")];
        app.sidebar_selection = 0;
        app.session = Some("c1".into());
        app.pending_load = Some("c1".into());
        let out = render(&app, 60, 12);
        let line = out
            .lines()
            .find(|l| l.contains("weekly planning"))
            .expect("row renders");
        assert!(line.contains('*'), "active star: {line}");
        assert!(line.contains('…'), "loading ellipsis: {line}");
        // Tree glyphs appear once a child exists.
        let mut app = test_support::app();
        app.focus = Focus::SessionBrowser;
        app.conversations = vec![
            conv("root", "root thread"),
            ConvHeader {
                id: "kid".into(),
                title: Some("child".into()),
                created_at: "2025-06-25T12:00:00Z".into(),
                parent_conversation: Some("root".into()),
                spawned_by: Some("m".into()),
            },
        ];
        let out = render(&app, 60, 12);
        assert!(out.contains('└'), "child glyph:\n{out}");
        assert!(out.contains('│'), "ancestor glyph:\n{out}");
    }
}
