use liberado_markdown::{self, MarkdownLine};
use liberado_theme::Theme;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::app::{App, Focus, Message};
use crate::format::{short_id, truncate_for_display};
use crate::tuning::*;
use crate::ui::c;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App, th: &Theme, spinner_tick: u8) {
    let title = if let Some(ref id) = app.session {
        format!(" Chat — {}", short_id(id))
    } else {
        " Chat — new conversation".to_string()
    };

    let focus_hint = if app.focus == Focus::ChatMessages {
        " [j/k navigate, Enter expand]"
    } else {
        ""
    };
    let title = format!("{title}{focus_hint}");

    let border_color = if app.focus == Focus::ChatMessages {
        c(&th.sidebar_border_focused, "#00ffff")
    } else {
        c(&th.border, "#808080")
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().fg(border_color));

    // If a conversation is being loaded and we have no messages yet, show a spinner.
    if app.pending_load.is_some() && app.messages.is_empty() {
        let spinner = SPINNER_FRAMES[(spinner_tick as usize) % SPINNER_FRAMES.len()];
        let loading_text = format!(" Loading conversation {spinner}");
        let loading = Paragraph::new(loading_text)
            .block(block)
            .style(Style::default().fg(c(&th.chat_system_text, "#808080")));
        frame.render_widget(loading, area);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();

    for (i, msg) in app.messages.iter().enumerate() {
        let is_selected = app.focus == Focus::ChatMessages && i == app.chat_cursor;
        let is_expanded = app.expanded_messages.contains(&i);
        let sel_bg = if is_selected {
            c(&th.sidebar_selected_bg, "#00ffff")
        } else {
            Color::Reset
        };
        let sel_fg = if is_selected {
            c(&th.sidebar_selected_fg, "#000000")
        } else {
            Color::Reset
        };

        match msg {
            Message::User(text) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "> ",
                        Style::default()
                            .fg(c(&th.chat_user_prefix, "#00ffff"))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        text.clone(),
                        Style::default().fg(c(&th.chat_user_text, "#ffffff")),
                    ),
                ]));
            }
            Message::Assistant(text) => {
                let md_lines = liberado_markdown::markdown_to_lines(text);
                for md in md_lines {
                    match md {
                        MarkdownLine::Paragraph(spans) => {
                            let line_spans: Vec<Span> = spans
                                .iter()
                                .map(|s| {
                                    let mut style = Style::default().fg(c(
                                        &th.chat_assistant_text,
                                        "#c0c0c0",
                                    ));
                                    if s.style.bold {
                                        style = style
                                            .fg(c(&th.md_bold, "#ffffff"))
                                            .add_modifier(Modifier::BOLD);
                                    }
                                    if s.style.italic {
                                        style = style
                                            .fg(c(&th.md_italic, "#c0c0c0"))
                                            .add_modifier(Modifier::ITALIC);
                                    }
                                    if s.style.code {
                                        style = Style::default()
                                            .fg(c(&th.md_code, "#ffff00"));
                                    }
                                    if s.style.link {
                                        style = Style::default()
                                            .fg(c(&th.md_link, "#8080ff"))
                                            .add_modifier(Modifier::UNDERLINED);
                                    }
                                    Span::styled(s.text.clone(), style)
                                })
                                .collect();
                            if !line_spans.is_empty() {
                                lines.push(Line::from(line_spans));
                            }
                        }
                        MarkdownLine::CodeBlock {
                            language,
                            lines: code,
                        } => {
                            let lang = language.unwrap_or_default();
                            let header = if lang.is_empty() {
                                "```".into()
                            } else {
                                format!("```{}", lang)
                            };
                            lines.push(Line::from(Span::styled(
                                header,
                                Style::default()
                                    .fg(c(&th.code_block_header, "#808000"))
                                    .add_modifier(Modifier::DIM),
                            )));
                            for cl in &code {
                                lines.push(Line::from(Span::styled(
                                    cl.clone(),
                                    Style::default()
                                        .fg(c(&th.code_block_fg, "#c0c0c0"))
                                        .bg(c(&th.code_block_bg, "#303030")),
                                )));
                            }
                            lines.push(Line::from(Span::styled(
                                "```",
                                Style::default()
                                    .fg(c(&th.code_block_header, "#808000"))
                                    .add_modifier(Modifier::DIM),
                            )));
                        }
                        MarkdownLine::Bullet(item) => {
                            lines.push(Line::from(vec![
                                Span::styled(
                                    "  • ",
                                    Style::default().fg(c(&th.md_bullet, "#00ffff")),
                                ),
                                Span::styled(
                                    item,
                                    Style::default()
                                        .fg(c(&th.chat_assistant_text, "#c0c0c0")),
                                ),
                            ]));
                        }
                        MarkdownLine::Heading(level, text) => {
                            let bold = if level <= HEADING_BOLD_THRESHOLD {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            };
                            lines.push(Line::from(Span::styled(
                                text,
                                Style::default()
                                    .fg(c(&th.md_heading, "#ffffff"))
                                    .add_modifier(bold),
                            )));
                        }
                        MarkdownLine::HorizontalRule => {
                            lines.push(Line::from(Span::styled(
                                "───",
                                Style::default().fg(c(&th.md_rule, "#404040")),
                            )));
                        }
                        MarkdownLine::Blank => {
                            lines.push(Line::from(""));
                        }
                    }
                }
            }
            Message::ToolCall(chip) => {
                let arrow = if is_expanded { "▼" } else { "▶" };
                let bg = sel_bg;
                lines.push(Line::from(vec![
                    Span::styled(
                        arrow,
                        Style::default()
                            .fg(c(&th.tool_ok, "#00ff00"))
                            .bg(bg),
                    ),
                    Span::styled(
                        " [tool] ",
                        Style::default()
                            .fg(c(&th.tool_label, "#ffff00"))
                            .add_modifier(Modifier::BOLD)
                            .bg(bg),
                    ),
                    Span::styled(
                        chip.name.clone(),
                        Style::default()
                            .fg(if is_selected {
                                sel_fg
                            } else {
                                c(&th.tool_name, "#ffff00")
                            })
                            .bg(bg),
                    ),
                    Span::styled(
                        if is_expanded {
                            chip.args.clone()
                        } else {
                            format!(
                                "({})",
                                truncate_for_display(&chip.args, TOOL_DISPLAY_TRUNCATE)
                            )
                        },
                        Style::default()
                            .fg(if is_selected {
                                sel_fg
                            } else {
                                c(&th.tool_args, "#808080")
                            })
                            .bg(bg),
                    ),
                ]));
            }
            Message::ToolResult(chip) => {
                let arrow = if is_expanded { "▼" } else { "▶" };
                let status = if chip.ok { "ok" } else { "err" };
                let status_color = if chip.ok {
                    c(&th.tool_ok, "#00ff00")
                } else {
                    c(&th.tool_err, "#ff0000")
                };
                let bg = sel_bg;
                lines.push(Line::from(vec![
                    Span::styled(
                        arrow,
                        Style::default()
                            .fg(c(&th.tool_ok, "#00ff00"))
                            .bg(bg),
                    ),
                    Span::styled(
                        " [tool] ",
                        Style::default()
                            .fg(c(&th.tool_label, "#ffff00"))
                            .add_modifier(Modifier::BOLD)
                            .bg(bg),
                    ),
                    Span::styled(
                        chip.name.clone(),
                        Style::default()
                            .fg(if is_selected {
                                sel_fg
                            } else {
                                c(&th.tool_name, "#ffff00")
                            })
                            .bg(bg),
                    ),
                    Span::raw(" "),
                    Span::styled(status, Style::default().fg(status_color).bg(bg)),
                    Span::styled(
                        if is_expanded {
                            format!(" {}", &chip.preview)
                        } else {
                            format!(
                                " {}",
                                truncate_for_display(&chip.preview, TOOL_DISPLAY_TRUNCATE)
                            )
                        },
                        Style::default()
                            .fg(if is_selected {
                                sel_fg
                            } else {
                                c(&th.tool_args, "#808080")
                            })
                            .bg(bg),
                    ),
                ]));
            }
            Message::System(text) => {
                for line in text.lines() {
                    lines.push(Line::from(Span::styled(
                        line.to_string(),
                        Style::default()
                            .fg(c(&th.chat_system_text, "#808080"))
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
            }
        }
    }

    if app.streaming || !app.assistant_buf.is_empty() {
        if !app.assistant_buf.is_empty() {
            lines.push(Line::from(Span::styled(
                app.assistant_buf.clone(),
                Style::default()
                    .fg(c(&th.chat_assistant_text, "#c0c0c0"))
                    .add_modifier(Modifier::ITALIC),
            )));
        }
        if app.streaming {
            lines.push(Line::from(Span::styled(
                "▌",
                Style::default()
                    .fg(c(&th.chat_streaming_cursor, "#00ffff"))
                    .add_modifier(Modifier::SLOW_BLINK),
            )));
        }
    }

    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.scroll_offset.min(u16::MAX as usize) as u16, 0));

    frame.render_widget(paragraph, area);
}
