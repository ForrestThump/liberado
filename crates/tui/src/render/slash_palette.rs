//! Floating slash-command palette rendered above the input box.

use liberado_theme::Theme;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem},
};

use crate::app::{App, Focus};
use crate::ui::c;

const MAX_VISIBLE: usize = 8;

pub(super) fn draw(frame: &mut Frame, input_area: Rect, app: &App, th: &Theme) {
    if app.focus != Focus::Input {
        return;
    }
    let matches = app.slash_matches();
    if matches.is_empty() {
        return;
    }

    let visible_n = matches.len().min(MAX_VISIBLE);
    let height = (visible_n as u16).saturating_add(2); // borders
    if height == 0 || input_area.y < height {
        return;
    }

    let width = input_area.width.min(72).max(24);
    let x = input_area.x;
    let y = input_area.y.saturating_sub(height);
    let area = Rect {
        x,
        y,
        width,
        height,
    };

    frame.render_widget(Clear, area);

    let border = c(&th.input_border_focused, "#00ffff");
    let text_fg = c(&th.sidebar_text, "#c0c0c0");
    let sel_fg = c(&th.sidebar_selected_fg, "#000000");
    let sel_bg = c(&th.sidebar_selected_bg, "#00ffff");
    let dim = c(&th.chat_system_text, "#808080");
    let bg = c(&th.app_bg, "#0d0d1a");

    // Scroll window so selection stays visible.
    let sel = app.slash_palette_index.min(matches.len().saturating_sub(1));
    let mut start = sel.saturating_sub(MAX_VISIBLE.saturating_sub(1));
    if start + visible_n > matches.len() {
        start = matches.len().saturating_sub(visible_n);
    }
    let end = (start + visible_n).min(matches.len());

    let items: Vec<ListItem> = matches[start..end]
        .iter()
        .enumerate()
        .map(|(i, spec)| {
            let idx = start + i;
            let selected = idx == sel;
            let label = format!(" {:18} {}", spec.name, spec.description);
            let style = if selected {
                Style::default().fg(sel_fg).bg(sel_bg).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(text_fg).bg(bg)
            };
            ListItem::new(Line::from(Span::styled(label, style)))
        })
        .collect();

    let title = if matches.len() > MAX_VISIBLE {
        format!(" Commands ({}/{}) · ↑↓ Tab ", sel + 1, matches.len())
    } else {
        " Commands · ↑↓ · Enter run · Tab fill ".into()
    };

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(border))
            .style(Style::default().bg(bg)),
    );

    // Hint line is the title; dim unused.
    let _ = dim;
    frame.render_widget(list, area);
}
