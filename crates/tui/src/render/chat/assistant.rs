use liberado_markdown::StyledSpan;

use super::*;

pub(super) fn assistant_span(span: &StyledSpan, th: &Theme) -> Span<'static> {
    let mut style = Style::default().fg(c(&th.chat_assistant_text, "#c0c0c0"));
    if span.style.bold {
        style = style
            .fg(c(&th.md_bold, "#ffffff"))
            .add_modifier(Modifier::BOLD);
    }
    if span.style.italic {
        style = style
            .fg(c(&th.md_italic, "#c0c0c0"))
            .add_modifier(Modifier::ITALIC);
    }
    if span.style.code {
        style = Style::default().fg(c(&th.md_code, "#ffff00"));
    }
    if span.style.link {
        style = Style::default()
            .fg(c(&th.md_link, "#8080ff"))
            .add_modifier(Modifier::UNDERLINED);
    }
    Span::styled(span.text.clone(), style)
}

pub(super) fn push_assistant_code_block(
    lines: &mut Vec<Line>,
    language: Option<&str>,
    code: &[String],
    th: &Theme,
) {
    let language = language.unwrap_or("");
    let header = if language.is_empty() {
        "```".into()
    } else {
        format!("```{language}")
    };
    lines.push(Line::from(Span::styled(
        header,
        Style::default()
            .fg(c(&th.code_block_header, "#808000"))
            .add_modifier(Modifier::DIM),
    )));
    for line in code {
        lines.push(Line::from(Span::styled(
            line.clone(),
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

pub(super) fn push_assistant_heading(lines: &mut Vec<Line>, level: usize, text: &str, th: &Theme) {
    let bold = if level <= HEADING_BOLD_THRESHOLD {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };
    lines.push(Line::from(Span::styled(
        text.to_string(),
        Style::default()
            .fg(c(&th.md_heading, "#ffffff"))
            .add_modifier(bold),
    )));
}
