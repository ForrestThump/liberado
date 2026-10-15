//! HTML for Matrix `formatted_body`. Every character of the source text is escaped.

use super::{MarkdownLine, StyledSpan, markdown_to_lines};

/// Render markdown as a fragment of HTML. The result contains no unescaped source text.
pub fn markdown_to_html(text: &str) -> String {
    let mut html = String::new();
    let mut list_open = false;
    for line in markdown_to_lines(text) {
        list_open = push_line(&mut html, list_open, &line);
    }
    close_list(&mut html, list_open);
    html
}

/// Escape `&`, `<`, `>`, and `"` so source text cannot break out of the HTML.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn push_line(html: &mut String, list_open: bool, line: &MarkdownLine) -> bool {
    match line {
        MarkdownLine::Bullet(item) => push_bullet(html, list_open, item),
        other => {
            close_list(html, list_open);
            push_block(html, other);
            false
        }
    }
}

fn push_bullet(html: &mut String, list_open: bool, item: &str) -> bool {
    if !list_open {
        html.push_str("<ul>");
    }
    html.push_str("<li>");
    html.push_str(&escape_html(item));
    html.push_str("</li>");
    true
}

fn close_list(html: &mut String, list_open: bool) {
    if list_open {
        html.push_str("</ul>");
    }
}

fn push_block(html: &mut String, line: &MarkdownLine) {
    match line {
        MarkdownLine::Paragraph(spans) => push_paragraph(html, spans),
        MarkdownLine::CodeBlock { lines, .. } => push_code_block(html, lines),
        MarkdownLine::Heading(level, text) => push_heading(html, *level, text),
        MarkdownLine::HorizontalRule => html.push_str("<hr>"),
        MarkdownLine::Blank | MarkdownLine::Bullet(_) => {}
    }
}

fn push_paragraph(html: &mut String, spans: &[StyledSpan]) {
    html.push_str("<p>");
    for span in spans {
        push_span(html, span);
    }
    html.push_str("</p>");
}

fn push_span(html: &mut String, span: &StyledSpan) {
    let text = escape_html(&span.text);
    if span.style.bold {
        wrap(html, "strong", &text);
    } else if span.style.italic {
        wrap(html, "em", &text);
    } else if span.style.code {
        wrap(html, "code", &text);
    } else {
        html.push_str(&text);
    }
}

fn wrap(html: &mut String, tag: &str, text: &str) {
    html.push('<');
    html.push_str(tag);
    html.push('>');
    html.push_str(text);
    html.push_str("</");
    html.push_str(tag);
    html.push('>');
}

fn push_code_block(html: &mut String, lines: &[String]) {
    html.push_str("<pre><code>");
    html.push_str(&escape_html(&lines.join("\n")));
    html.push_str("</code></pre>");
}

fn push_heading(html: &mut String, level: usize, text: &str) {
    let level = level.clamp(1, 3);
    html.push_str(&format!("<h{level}>"));
    html.push_str(&escape_html(text));
    html.push_str(&format!("</h{level}>"));
}

#[cfg(test)]
#[path = "html_tests.rs"]
mod tests;
