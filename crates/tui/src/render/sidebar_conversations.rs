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
    let border_color = if app.focus == crate::app::Focus::SidebarConversations {
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
            let display = node.header.title.as_deref().unwrap_or("(untitled)");
            let id_short = short_id(&node.header.id);
            let rel = relative_time(&node.header.created_at);
            let active = app.session.as_deref() == Some(node.header.id.as_str());
            let is_selected =
                i == app.sidebar_selection && app.focus == crate::app::Focus::SidebarConversations;
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
