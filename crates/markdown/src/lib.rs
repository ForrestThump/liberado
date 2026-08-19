//! Lightweight Markdown parser for Liberado — UI-agnostic blocks and inline spans.
//!
//! No external parser dependency. Returns abstract [`MarkdownLine`] and [`SpanStyle`]
//! types that any renderer (ratatui, HTML/Dioxus, terminal escapes) can map to its own
//! primitives.
//!
//! ## Supported syntax
//!
//! | Feature          | Example                     |
//! |------------------|-----------------------------|
//! | Bold             | `**text**`                  |
//! | Italic           | `*text*`                    |
//! | Inline code      | `` `code` ``                |
//! | Links            | `[text](url)`               |
//! | Fenced code block| ` ```lang\ncode\n``` `      |
//! | Bullet list      | `- item` or `* item`        |
//! | Headings         | `## Heading` (h1-h3)        |
//! | Horizontal rule  | `---` or `***`              |

/// A styled span of inline text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    pub text: String,
    pub style: SpanStyle,
}

/// Inline style flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpanStyle {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: bool,
}

impl SpanStyle {
    pub const NONE: SpanStyle = SpanStyle {
        bold: false,
        italic: false,
        code: false,
        link: false,
    };
    pub const BOLD: SpanStyle = SpanStyle {
        bold: true,
        italic: false,
        code: false,
        link: false,
    };
    pub const ITALIC: SpanStyle = SpanStyle {
        bold: false,
        italic: true,
        code: false,
        link: false,
    };
    pub const CODE: SpanStyle = SpanStyle {
        bold: false,
        italic: false,
        code: true,
        link: false,
    };
    pub const LINK: SpanStyle = SpanStyle {
        bold: false,
        italic: false,
        code: false,
        link: true,
    };
}

/// A block-level markdown element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownLine {
    /// A paragraph of styled spans.
    Paragraph(Vec<StyledSpan>),
    /// A fenced code block.
    CodeBlock {
        language: Option<String>,
        lines: Vec<String>,
    },
    /// A single bullet list item.
    Bullet(String),
    /// A heading at the given level (1-3).
    Heading(usize, String),
    /// A horizontal rule.
    HorizontalRule,
    /// An empty line between blocks.
    Blank,
}

/// Parse markdown text into a vector of abstract `MarkdownLine`s.
pub fn markdown_to_lines(text: &str) -> Vec<MarkdownLine> {
    let mut result = Vec::new();
    let mut in_code_block = false;
    let mut code_language: Option<String> = None;
    let mut code_lines: Vec<String> = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        let trimmed_end = line.trim_end();

        if !in_code_block && trimmed.starts_with("```") {
            in_code_block = true;
            let lang = trimmed.strip_prefix("```").unwrap_or("").trim();
            code_language = if lang.is_empty() {
                None
            } else {
                Some(lang.to_string())
            };
            code_lines.clear();
            continue;
        }

        if in_code_block {
            if trimmed == "```" {
                in_code_block = false;
                result.push(MarkdownLine::CodeBlock {
                    language: code_language.take(),
                    lines: std::mem::take(&mut code_lines),
                });
                continue;
            }
            code_lines.push(if !trimmed.is_empty() {
                line.to_string()
            } else {
                String::new()
            });
            continue;
        }

        if trimmed.is_empty() {
            result.push(MarkdownLine::Blank);
            continue;
        }

        if trimmed.starts_with("### ") {
            result.push(MarkdownLine::Heading(
                3,
                trimmed.strip_prefix("### ").unwrap().to_string(),
            ));
            continue;
        }
        if trimmed.starts_with("## ") {
            result.push(MarkdownLine::Heading(
                2,
                trimmed.strip_prefix("## ").unwrap().to_string(),
            ));
            continue;
        }
        if trimmed.starts_with("# ") {
            result.push(MarkdownLine::Heading(
                1,
                trimmed.strip_prefix("# ").unwrap().to_string(),
            ));
            continue;
        }

        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            result.push(MarkdownLine::HorizontalRule);
            continue;
        }

        if let Some(content) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            result.push(MarkdownLine::Bullet(content.to_string()));
            continue;
        }

        result.push(MarkdownLine::Paragraph(parse_inline(trimmed_end)));
    }

    if in_code_block {
        result.push(MarkdownLine::CodeBlock {
            language: code_language,
            lines: code_lines,
        });
    }

    result
}

/// Try to parse a bold span (`**…**`) starting at `i`. `Some((next, span))` when found.
fn parse_bold(text: &str, i: usize) -> Option<(usize, StyledSpan)> {
    let bytes = text.as_bytes();
    if bytes[i] == b'*'
        && i + 1 < bytes.len()
        && bytes[i + 1] == b'*'
        && let Some(end_idx) = find_inline_end(text, i + 2, "**")
    {
        let inner = &text[i + 2..end_idx];
        return Some((
            end_idx + 2,
            StyledSpan {
                text: inner.to_string(),
                style: SpanStyle::BOLD,
            },
        ));
    }
    None
}

/// Try to parse an italic span (`*…*`) starting at `i`. Only the first star of `**` is eligible.
fn parse_italic(text: &str, i: usize) -> Option<(usize, StyledSpan)> {
    let bytes = text.as_bytes();
    if bytes[i] == b'*'
        && (i == 0 || bytes[i - 1] != b'*')
        && let Some(end_idx) = find_inline_end(text, i + 1, "*")
    {
        let inner = &text[i + 1..end_idx];
        return Some((
            end_idx + 1,
            StyledSpan {
                text: inner.to_string(),
                style: SpanStyle::ITALIC,
            },
        ));
    }
    None
}

/// Try to parse a link span (`[text](url)`) starting at `i`. The URL is validated only as far as
/// the balanced paren; it is not dereferenced here.
fn parse_link(text: &str, i: usize) -> Option<(usize, StyledSpan)> {
    let bytes = text.as_bytes();
    if bytes[i] == b'['
        && let Some(bracket_end) = find_matching_bracket(text, i)
        && bracket_end + 1 < bytes.len()
        && bytes[bracket_end + 1] == b'('
        && let Some(paren_end) = find_matching_paren(text, bracket_end + 1)
    {
        let link_text = &text[i + 1..bracket_end];
        return Some((
            paren_end + 1,
            StyledSpan {
                text: link_text.to_string(),
                style: SpanStyle::LINK,
            },
        ));
    }
    None
}

/// Try to parse a code span (`` `…` ``) starting at `i`.
fn parse_code(text: &str, i: usize) -> Option<(usize, StyledSpan)> {
    if text.as_bytes()[i] == b'`'
        && let Some(end_idx) = find_inline_end(text, i + 1, "`")
    {
        let inner = &text[i + 1..end_idx];
        return Some((
            end_idx + 1,
            StyledSpan {
                text: inner.to_string(),
                style: SpanStyle::CODE,
            },
        ));
    }
    None
}

fn parse_inline(text: &str) -> Vec<StyledSpan> {
    let mut spans = Vec::new();
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if let Some((next, span)) = parse_bold(text, i).or_else(|| parse_italic(text, i)) {
            spans.push(span);
            i = next;
            continue;
        }
        if let Some((next, span)) = parse_link(text, i).or_else(|| parse_code(text, i)) {
            spans.push(span);
            i = next;
            continue;
        }

        let start = i;
        while i < len && bytes[i] != b'*' && bytes[i] != b'[' && bytes[i] != b'`' {
            i += 1;
        }
        if i > start {
            spans.push(StyledSpan {
                text: text[start..i].to_string(),
                style: SpanStyle::NONE,
            });
        }
    }

    spans
}

fn find_inline_end(text: &str, start: usize, marker: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let marker_bytes = marker.as_bytes();
    let mlen = marker_bytes.len();
    let mut i = start;
    while i + mlen <= bytes.len() {
        if &bytes[i..i + mlen] == marker_bytes {
            let before = if i > 0 { bytes[i - 1] } else { 0 };
            if before != b'\\' {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn find_matching_bracket(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0;
    for (i, &b) in bytes.iter().enumerate().skip(open + 1) {
        match b {
            b'[' => depth += 1,
            b']' if depth == 0 => return Some(i),
            b']' => depth -= 1,
            _ => {}
        }
    }
    None
}

fn find_matching_paren(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0;
    for (i, &b) in bytes.iter().enumerate().skip(open + 1) {
        match b {
            b'(' => depth += 1,
            b')' if depth == 0 => return Some(i),
            b')' => depth -= 1,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_paragraph() {
        let lines = markdown_to_lines("hello world");
        assert_eq!(lines.len(), 1);
        match &lines[0] {
            MarkdownLine::Paragraph(spans) => {
                assert_eq!(spans[0].text, "hello world");
                assert_eq!(spans[0].style, SpanStyle::NONE);
            }
            _ => panic!("expected Paragraph"),
        }
    }

    #[test]
    fn bold_inline() {
        let lines = markdown_to_lines("this is **bold** text");
        match &lines[0] {
            MarkdownLine::Paragraph(spans) => {
                assert_eq!(spans.len(), 3);
                assert_eq!(spans[0].text, "this is ");
                assert_eq!(spans[0].style, SpanStyle::NONE);
                assert_eq!(spans[1].text, "bold");
                assert_eq!(spans[1].style, SpanStyle::BOLD);
                assert_eq!(spans[2].text, " text");
                assert_eq!(spans[2].style, SpanStyle::NONE);
            }
            _ => panic!("expected Paragraph"),
        }
    }

    #[test]
    fn italic_inline() {
        let lines = markdown_to_lines("some *italic* here");
        match &lines[0] {
            MarkdownLine::Paragraph(spans) => {
                assert!(
                    spans
                        .iter()
                        .any(|s| s.text == "italic" && s.style == SpanStyle::ITALIC)
                );
            }
            _ => panic!(),
        }
    }

    #[test]
    fn inline_code() {
        let lines = markdown_to_lines("use `foo.bar()` function");
        match &lines[0] {
            MarkdownLine::Paragraph(spans) => {
                assert!(
                    spans
                        .iter()
                        .any(|s| s.text == "foo.bar()" && s.style == SpanStyle::CODE)
                );
            }
            _ => panic!(),
        }
    }

    #[test]
    fn link_inline() {
        let lines = markdown_to_lines("see [docs](https://example.com) for more");
        match &lines[0] {
            MarkdownLine::Paragraph(spans) => {
                assert!(
                    spans
                        .iter()
                        .any(|s| s.text == "docs" && s.style == SpanStyle::LINK)
                );
            }
            _ => panic!(),
        }
    }

    #[test]
    fn fenced_code_block() {
        let input = "before\n```rust\nfn main() {}\n```\nafter";
        let lines = markdown_to_lines(input);
        assert_eq!(lines.len(), 3);
        match &lines[1] {
            MarkdownLine::CodeBlock {
                language,
                lines: code,
            } => {
                assert_eq!(language.as_deref(), Some("rust"));
                assert_eq!(code, &["fn main() {}"]);
            }
            _ => panic!("expected CodeBlock, got {:?}", lines[1]),
        }
        assert!(matches!(&lines[0], MarkdownLine::Paragraph(_)));
        assert!(matches!(&lines[2], MarkdownLine::Paragraph(_)));
    }

    #[test]
    fn bullet_list() {
        let input = "- item one\n* item two\n- item three";
        let lines = markdown_to_lines(input);
        assert_eq!(lines.len(), 3);
        assert!(matches!(&lines[0], MarkdownLine::Bullet(s) if s == "item one"));
        assert!(matches!(&lines[1], MarkdownLine::Bullet(s) if s == "item two"));
        assert!(matches!(&lines[2], MarkdownLine::Bullet(s) if s == "item three"));
    }

    #[test]
    fn headings() {
        let input = "# H1\n## H2\n### H3";
        let lines = markdown_to_lines(input);
        assert!(matches!(&lines[0], MarkdownLine::Heading(1, s) if s == "H1"));
        assert!(matches!(&lines[1], MarkdownLine::Heading(2, s) if s == "H2"));
        assert!(matches!(&lines[2], MarkdownLine::Heading(3, s) if s == "H3"));
    }

    #[test]
    fn horizontal_rule() {
        let input = "text\n---\nmore";
        let lines = markdown_to_lines(input);
        assert!(matches!(&lines[1], MarkdownLine::HorizontalRule));
    }

    #[test]
    fn unclosed_code_block_included() {
        let input = "```\norphaned code";
        let lines = markdown_to_lines(input);
        match &lines[0] {
            MarkdownLine::CodeBlock { lines: code, .. } => {
                assert_eq!(code, &["orphaned code"]);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn blank_lines_between_blocks() {
        let input = "para1\n\npara2";
        let lines = markdown_to_lines(input);
        assert_eq!(lines.len(), 3);
        assert!(matches!(lines[1], MarkdownLine::Blank));
    }
}
