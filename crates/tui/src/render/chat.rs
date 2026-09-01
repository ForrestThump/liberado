use liberado_markdown::{self, MarkdownLine};
use liberado_theme::Theme;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::api::{ToolCallChip, ToolResultChip};
use crate::app::{App, Focus, JoinedSession, Message};
use crate::format::{short_id, truncate_for_display};
use crate::md_cache::MarkdownParseCache;
use crate::render::kind_color;
use crate::tuning::*;
use crate::ui::c;

mod assistant;
use assistant::{assistant_span, push_assistant_code_block, push_assistant_heading};

/// The joined view, snapshot as owned clones so the render loop can borrow `app.md_cache`
/// mutably below. The primary-chat path stays zero-copy (borrows `app.messages`); only the cold
/// joined path clones, and a specialist transcript is small.
struct JoinedSnapshot {
    joined_active: bool,
    joined_finished: bool,
    messages_owned: Option<Vec<Message>>,
    stream_buf: String,
    awaiting: Option<(String, Vec<String>)>,
}

fn snapshot_joined_view(app: &App) -> JoinedSnapshot {
    JoinedSnapshot {
        joined_active: app.joined.is_some(),
        joined_finished: app.joined.as_ref().map(|j| j.finished).unwrap_or(false),
        messages_owned: app.joined.as_ref().map(|j| j.messages.clone()),
        stream_buf: app
            .joined
            .as_ref()
            .map(|j| j.stream_buf.clone())
            .unwrap_or_default(),
        awaiting: app.joined.as_ref().and_then(|j| {
            j.awaiting
                .as_ref()
                .map(|a| (a.prompt.clone(), a.options.clone()))
        }),
    }
}

/// Selection/expansion only applies to the primary conversation view (chat-history focus
/// navigates `app.messages`); the joined transcript renders read-only.
fn selection_for(app: &App, i: usize, joined_active: bool) -> (bool, bool) {
    let is_selected = !joined_active && app.focus == Focus::ChatMessages && i == app.chat_cursor;
    let is_expanded = app.expanded_messages.contains(&i);
    (is_selected, is_expanded)
}

fn selection_colors(is_selected: bool, th: &Theme, pane_bg: Color) -> (Color, Color) {
    if is_selected {
        (
            c(&th.sidebar_selected_bg, "#00ffff"),
            c(&th.sidebar_selected_fg, "#000000"),
        )
    } else {
        (pane_bg, Color::Reset)
    }
}

/// Joined session: render any buffered stream tokens, then the "awaiting your reply" prompt as a
/// highlighted banner so it's unmistakable that the input box is now feeding this session.
fn push_joined_tail(lines: &mut Vec<Line>, th: &Theme, snap: &JoinedSnapshot) {
    if !snap.stream_buf.is_empty() {
        lines.push(Line::from(Span::styled(
            snap.stream_buf.clone(),
            Style::default()
                .fg(c(&th.chat_assistant_text, "#c0c0c0"))
                .add_modifier(Modifier::ITALIC),
        )));
    }
    if let Some((prompt, options)) = &snap.awaiting {
        lines.push(Line::from(""));
        // One `Line` per source line: a ratatui `Line` does not break on `\n`, and prompts are
        // not always one-liners — an intake draft contract (S7) is a whole block of criteria and
        // verifiers. Cramming it into a single Line would run it all together.
        let accent = Style::default()
            .fg(c(&th.accent, "#00ffff"))
            .add_modifier(Modifier::BOLD);
        for (n, raw) in prompt.lines().enumerate() {
            let text = if n == 0 {
                format!("❓ {raw}")
            } else {
                format!("   {raw}")
            };
            lines.push(Line::from(Span::styled(text, accent)));
        }
        for (n, opt) in options.iter().enumerate() {
            lines.push(Line::from(Span::styled(
                format!("   {}. {opt}", n + 1),
                Style::default().fg(c(&th.md_bullet, "#00ffff")),
            )));
        }
        lines.push(Line::from(Span::styled(
            "   › type your answer below and press Enter",
            Style::default()
                .fg(c(&th.chat_system_text, "#808080"))
                .add_modifier(Modifier::ITALIC),
        )));
    } else if snap.joined_finished {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "— session finished · /back to return to the primary chat —",
            Style::default()
                .fg(c(&th.chat_system_text, "#808080"))
                .add_modifier(Modifier::ITALIC),
        )));
    }
}

/// Primary conversation: the streaming assistant buffer (italic) plus the blinking cursor.
fn push_primary_tail(lines: &mut Vec<Line>, th: &Theme, app: &App) {
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
}

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, th: &Theme, spinner_tick: u8) {
    // Unified-Session view: when joined to a goal session, the chat pane renders *that* session's
    // (separate-but-linked) transcript and identity; otherwise the primary conversation.
    let joined = app.joined.as_ref();
    let block = pane_block(joined, app, th);
    let snapshot = snapshot_joined_view(app);
    let joined_active = snapshot.joined_active;

    // If a conversation is being loaded and we have no messages yet, show a spinner.
    if !joined_active && app.pending_load.is_some() && app.messages.is_empty() {
        let spinner = SPINNER_FRAMES[(spinner_tick as usize) % SPINNER_FRAMES.len()];
        let loading_text = format!(" Loading conversation {spinner}");
        let loading = Paragraph::new(loading_text)
            .block(block)
            .style(Style::default().fg(c(&th.chat_system_text, "#808080")));
        frame.render_widget(loading, area);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();

    // Explicit theme bg for chips — `Color::Reset` leaks the host console palette
    // (e.g. Windows PowerShell blue) under any span that sets `.bg(...)`.
    let pane_bg = c(&th.app_bg, "#0d0d1a");
    let chip_body_bg = c(&th.code_block_bg, "#303030");

    let messages: &[Message] = match &snapshot.messages_owned {
        Some(m) => m,
        None => &app.messages,
    };

    // Your turns are numbered so `/fork <n>` is something you can *point at* rather than count.
    // The count starts from `turn_offset`, not from zero, because a long history is pruned at the
    // top — numbering the survivors from 1 would put a "3" on screen that means something else to
    // the server. The joined (read-only) transcript isn't forkable, so it isn't numbered.
    let mut user_turn = app.turn_offset;

    for (i, msg) in messages.iter().enumerate() {
        let (is_selected, is_expanded) = selection_for(app, i, joined_active);
        let (sel_bg, sel_fg) = selection_colors(is_selected, th, pane_bg);

        match msg {
            Message::User(text) => push_user_message(
                &mut lines,
                text,
                th,
                is_selected,
                sel_bg,
                sel_fg,
                joined_active,
                &mut user_turn,
            ),
            Message::Assistant(text) => {
                push_assistant_message(&mut lines, text, th, &mut app.md_cache)
            }
            Message::ToolCall(chip) => push_tool_call(
                &mut lines,
                chip,
                th,
                is_selected,
                is_expanded,
                sel_bg,
                sel_fg,
                chip_body_bg,
            ),
            Message::ToolResult(chip) => push_tool_result(
                &mut lines,
                chip,
                th,
                is_selected,
                is_expanded,
                sel_bg,
                sel_fg,
                chip_body_bg,
            ),
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

    if joined_active {
        push_joined_tail(&mut lines, th, &snapshot);
    } else {
        push_primary_tail(&mut lines, th, app);
    }

    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.scroll_offset.min(u16::MAX as usize) as u16, 0));

    frame.render_widget(paragraph, area);
}

/// The chat pane's frame: title (session identity or primary-chat), focus hint, and border tint.
fn pane_block(joined: Option<&JoinedSession>, app: &App, th: &Theme) -> Block<'static> {
    let title = if let Some(j) = joined {
        let desc = if j.description.is_empty() {
            short_id(&j.id).to_string()
        } else {
            truncate_for_display(&j.description, 48)
        };
        let awaiting = if j.awaiting.is_some() {
            "  ⏳ awaiting your reply"
        } else {
            ""
        };
        format!(" {} — {} [{}]{awaiting}", j.kind.label(), desc, j.status)
    } else if let Some(ref id) = app.session {
        format!(" Chat — {}", short_id(id))
    } else {
        " Chat — new conversation".to_string()
    };

    // History-navigation hint only applies to the primary conversation view.
    let focus_hint = if joined.is_some() {
        " [/back to leave]"
    } else if app.focus == Focus::ChatMessages {
        " [j/k · Enter expand tools · f fork here · Esc back]"
    } else {
        " [Tab focus history]"
    };
    let title = format!("{title}{focus_hint}");

    let border_color = if let Some(j) = joined {
        kind_color(j.kind, th)
    } else if app.focus == Focus::ChatMessages {
        c(&th.sidebar_border_focused, "#00ffff")
    } else {
        c(&th.border, "#808080")
    };
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().fg(border_color))
}

#[allow(clippy::too_many_arguments)]
fn push_user_message(
    lines: &mut Vec<Line>,
    text: &str,
    th: &Theme,
    is_selected: bool,
    sel_bg: Color,
    sel_fg: Color,
    joined_active: bool,
    user_turn: &mut usize,
) {
    // Soft themed strip so user turns scan apart from agent output.
    let user_bg = if is_selected {
        sel_bg
    } else {
        c(&th.chat_user_bg, "#16162a")
    };
    let prefix_fg = if is_selected {
        sel_fg
    } else {
        c(&th.chat_user_prefix, "#00ffff")
    };
    let text_fg = if is_selected {
        sel_fg
    } else {
        c(&th.chat_user_text, "#ffffff")
    };
    // The turn number this message is, in the numbering `/fork <n>` uses. Read-only
    // transcripts aren't forkable, so they stay unnumbered.
    *user_turn += 1;
    let lead = if joined_active {
        "> ".to_string()
    } else {
        format!("{user_turn}> ")
    };
    // Continuation lines indent to exactly the width of the lead, whatever its digits.
    let cont = " ".repeat(lead.chars().count());

    // Multi-line user messages: paint every line with the same soft bg.
    let mut body_lines = text.lines().peekable();
    if body_lines.peek().is_none() {
        lines.push(Line::from(vec![
            Span::styled(
                lead,
                Style::default()
                    .fg(prefix_fg)
                    .bg(user_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", Style::default().bg(user_bg)),
        ]));
    } else {
        let mut first = true;
        for line in text.lines() {
            let (lead_text, bold) = if first {
                first = false;
                (lead.clone(), Modifier::BOLD)
            } else {
                (cont.clone(), Modifier::empty())
            };
            lines.push(Line::from(vec![
                Span::styled(
                    lead_text,
                    Style::default()
                        .fg(prefix_fg)
                        .bg(user_bg)
                        .add_modifier(bold),
                ),
                Span::styled(line.to_string(), Style::default().fg(text_fg).bg(user_bg)),
            ]));
        }
    }
}

fn push_assistant_message(
    lines: &mut Vec<Line>,
    text: &str,
    th: &Theme,
    md_cache: &mut MarkdownParseCache,
) {
    // T1.1: parse once per distinct body; redraws while streaming other turns hit cache.
    let md_lines = md_cache.get_or_parse(text);
    for md in md_lines.iter() {
        match md {
            MarkdownLine::Paragraph(spans) => {
                let line_spans: Vec<Span> =
                    spans.iter().map(|span| assistant_span(span, th)).collect();
                if !line_spans.is_empty() {
                    lines.push(Line::from(line_spans));
                }
            }
            MarkdownLine::CodeBlock {
                language,
                lines: code,
            } => push_assistant_code_block(lines, language.as_deref(), code, th),
            MarkdownLine::Bullet(item) => {
                lines.push(Line::from(vec![
                    Span::styled("  • ", Style::default().fg(c(&th.md_bullet, "#00ffff"))),
                    Span::styled(
                        item.clone(),
                        Style::default().fg(c(&th.chat_assistant_text, "#c0c0c0")),
                    ),
                ]));
            }
            MarkdownLine::Heading(level, text) => {
                push_assistant_heading(lines, *level, text, th);
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

#[allow(clippy::too_many_arguments)]
fn push_tool_call(
    lines: &mut Vec<Line>,
    chip: &ToolCallChip,
    th: &Theme,
    is_selected: bool,
    is_expanded: bool,
    sel_bg: Color,
    sel_fg: Color,
    chip_body_bg: Color,
) {
    let arrow = if is_expanded { "▼" } else { "▶" };
    let header_bg = sel_bg;
    let body_bg = if is_selected { sel_bg } else { chip_body_bg };
    let name_fg = if is_selected {
        sel_fg
    } else {
        c(&th.tool_name, "#ffff00")
    };
    let args_fg = if is_selected {
        sel_fg
    } else {
        c(&th.tool_args, "#808080")
    };
    lines.push(Line::from(vec![
        Span::styled(
            arrow,
            Style::default().fg(c(&th.tool_ok, "#00ff00")).bg(header_bg),
        ),
        Span::styled(
            " [tool] ",
            Style::default()
                .fg(c(&th.tool_label, "#ffff00"))
                .add_modifier(Modifier::BOLD)
                .bg(header_bg),
        ),
        Span::styled(
            chip.name.clone(),
            Style::default().fg(name_fg).bg(header_bg),
        ),
        Span::styled(
            if is_expanded {
                String::new()
            } else {
                format!(
                    "({})",
                    truncate_for_display(&chip.args, TOOL_DISPLAY_TRUNCATE)
                )
            },
            Style::default().fg(args_fg).bg(header_bg),
        ),
    ]));
    if is_expanded {
        for line in chip.args.lines() {
            lines.push(Line::from(Span::styled(
                format!("    {line}"),
                Style::default()
                    .fg(if is_selected {
                        sel_fg
                    } else {
                        c(&th.code_block_fg, "#c0c0c0")
                    })
                    .bg(body_bg),
            )));
        }
        if chip.args.is_empty() {
            lines.push(Line::from(Span::styled(
                "    (no args)",
                Style::default()
                    .fg(if is_selected {
                        sel_fg
                    } else {
                        c(&th.tool_args, "#808080")
                    })
                    .bg(body_bg),
            )));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_tool_result(
    lines: &mut Vec<Line>,
    chip: &ToolResultChip,
    th: &Theme,
    is_selected: bool,
    is_expanded: bool,
    sel_bg: Color,
    sel_fg: Color,
    chip_body_bg: Color,
) {
    let arrow = if is_expanded { "▼" } else { "▶" };
    let status = if chip.ok { "ok" } else { "err" };
    let status_color = if chip.ok {
        c(&th.tool_ok, "#00ff00")
    } else {
        c(&th.tool_err, "#ff0000")
    };
    let header_bg = sel_bg;
    let body_bg = if is_selected { sel_bg } else { chip_body_bg };
    let name_fg = if is_selected {
        sel_fg
    } else {
        c(&th.tool_name, "#ffff00")
    };
    let body_fg = if is_selected {
        sel_fg
    } else {
        c(&th.tool_args, "#808080")
    };
    let result_fg = if is_selected {
        sel_fg
    } else {
        c(&th.code_block_fg, "#c0c0c0")
    };
    lines.push(Line::from(vec![
        Span::styled(
            arrow,
            Style::default().fg(c(&th.tool_ok, "#00ff00")).bg(header_bg),
        ),
        Span::styled(
            " [tool] ",
            Style::default()
                .fg(c(&th.tool_label, "#ffff00"))
                .add_modifier(Modifier::BOLD)
                .bg(header_bg),
        ),
        Span::styled(
            chip.name.clone(),
            Style::default().fg(name_fg).bg(header_bg),
        ),
        Span::styled(" ", Style::default().bg(header_bg)),
        Span::styled(status, Style::default().fg(status_color).bg(header_bg)),
        Span::styled(
            if is_expanded {
                String::new()
            } else {
                format!(
                    " {}",
                    truncate_for_display(&chip.preview, TOOL_DISPLAY_TRUNCATE)
                )
            },
            Style::default().fg(body_fg).bg(header_bg),
        ),
    ]));
    if is_expanded {
        for line in chip.preview.lines() {
            lines.push(Line::from(Span::styled(
                format!("    {line}"),
                Style::default().fg(result_fg).bg(body_bg),
            )));
        }
        if chip.preview.is_empty() {
            lines.push(Line::from(Span::styled(
                "    (empty result)",
                Style::default().fg(body_fg).bg(body_bg),
            )));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ToolCallChip, ToolResultChip};
    use crate::app::{Focus, Message};
    use crate::render::test_support;

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let th = app.theme.clone();
        test_support::render_pane(w, h, |f| draw(f, Rect::new(0, 1, w, h), app, &th, 0))
    }

    #[test]
    fn empty_conversation_shows_a_new_conversation_title() {
        let mut app = test_support::app();
        let out = render(&mut app, 80, 20);
        assert!(out.contains("Chat — new conversation"), "title:\n{out}");
    }

    #[test]
    fn user_assistant_and_system_messages_render() {
        let mut app = test_support::app();
        app.messages = vec![
            Message::User("hello there".into()),
            Message::Assistant("hi! how can I help".into()),
            Message::System("connected".into()),
        ];
        app.scroll_offset = 0;
        let out = render(&mut app, 80, 20);
        assert!(out.contains("hello there"), "user:\n{out}");
        assert!(out.contains("hi! how can I help"), "assistant:\n{out}");
        assert!(out.contains("connected"), "system:\n{out}");
    }

    #[test]
    fn loading_state_shows_a_spinner_when_pending() {
        let mut app = test_support::app();
        app.pending_load = Some("c1".into());
        let out = render(&mut app, 60, 12);
        assert!(out.contains("Loading conversation"), "loading:\n{out}");
    }

    #[test]
    fn tool_chips_render_inline() {
        let mut app = test_support::app();
        app.messages = vec![
            Message::ToolCall(ToolCallChip {
                name: "search".into(),
                args: "{\"q\":\"stuff\"}".into(),
            }),
            Message::ToolResult(ToolResultChip {
                name: "search".into(),
                ok: true,
                preview: "3 results".into(),
            }),
        ];
        let out = render(&mut app, 100, 20);
        assert!(out.contains("search"), "tool name:\n{out}");
        assert!(out.contains("3 results"), "result preview:\n{out}");
    }

    #[test]
    fn joined_goal_session_title_and_awaiting_banner_render() {
        use crate::api::SessionKind;
        let mut app = test_support::app();
        app.joined = Some(crate::app::JoinedSession {
            id: "g1".into(),
            kind: SessionKind::Coding,
            status: "running".into(),
            finished: false,
            description: "build the CLI".into(),
            messages: vec![Message::Assistant("work in progress".into())],
            stream_buf: String::new(),
            awaiting: Some(crate::app::AwaitingPrompt {
                prompt: "choose an option".into(),
                options: vec!["A".into(), "B".into()],
            }),
            gate_votes: Vec::new(),
            active_role: None,
            last_validation: None,
        });
        let out = render(&mut app, 100, 20);
        assert!(out.contains("build the CLI"), "description:\n{out}");
        assert!(out.contains("Coding"), "kind label:\n{out}");
        assert!(out.contains("running"), "status:\n{out}");
        assert!(out.contains("awaiting"), "awaiting banner:\n{out}");
        assert!(
            out.contains("work in progress"),
            "joined transcript:\n{out}"
        );
        assert!(out.contains("/back to leave"), "leave hint:\n{out}");
    }

    #[test]
    fn history_focus_shows_navigation_hint() {
        let mut app = test_support::app();
        app.focus = Focus::ChatMessages;
        app.messages = vec![Message::User("x".into())];
        let out = render(&mut app, 100, 16);
        assert!(out.contains("j/k"), "nav hint:\n{out}");
    }

    /// The awaiting banner body — the prompt, its numbered options, and the answer hint — is a
    /// separate concern from the "⏳ awaiting" title hint; pin it so a dropped banner still fails.
    #[test]
    fn awaiting_banner_body_renders_prompt_options_and_hint() {
        use crate::api::SessionKind;
        let mut app = test_support::app();
        app.joined = Some(crate::app::JoinedSession {
            id: "g1".into(),
            kind: SessionKind::Coding,
            status: "running".into(),
            finished: false,
            description: "d".into(),
            messages: vec![],
            stream_buf: String::new(),
            awaiting: Some(crate::app::AwaitingPrompt {
                prompt: "pick one".into(),
                options: vec!["Left".into(), "Right".into()],
            }),
            gate_votes: Vec::new(),
            active_role: None,
            last_validation: None,
        });
        let out = render(&mut app, 100, 20);
        assert!(out.contains("pick one"), "prompt:\n{out}");
        assert!(out.contains("1. Left"), "option:\n{out}");
        assert!(out.contains("2. Right"), "option:\n{out}");
        assert!(out.contains("type your answer"), "hint:\n{out}");
    }

    /// A joined session that has finished shows the /back note instead of an awaiting banner.
    #[test]
    fn joined_finished_session_shows_back_note() {
        use crate::api::SessionKind;
        let mut app = test_support::app();
        app.joined = Some(crate::app::JoinedSession {
            id: "g1".into(),
            kind: SessionKind::Coding,
            status: "finished".into(),
            finished: true,
            description: "d".into(),
            messages: vec![],
            stream_buf: String::new(),
            awaiting: None,
            gate_votes: Vec::new(),
            active_role: None,
            last_validation: None,
        });
        let out = render(&mut app, 100, 20);
        assert!(out.contains("session finished"), "note:\n{out}");
        assert!(!out.contains("type your answer"), "no banner:\n{out}");
    }

    /// The streaming cursor and the in-flight assistant buffer render at the tail of the primary
    /// conversation view.
    #[test]
    fn primary_tail_renders_streaming_cursor_and_buffer() {
        let mut app = test_support::app();
        app.streaming = true;
        app.assistant_buf = "almost done".into();
        let out = render(&mut app, 100, 16);
        assert!(out.contains("almost done"), "buffer:\n{out}");
        assert!(out.contains("▌"), "cursor:\n{out}");
    }
}
