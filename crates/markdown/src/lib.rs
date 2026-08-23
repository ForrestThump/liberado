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
        && end_idx > i + 2
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
        && end_idx > i + 1
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
        && bracket_end > i + 1
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
        && end_idx > i + 1
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

/// Parse a paragraph body into styled spans. Always terminates: every iteration consumes at
/// least one byte, so malformed input with unterminated `*`, `[`, or `` ` `` markup renders
/// literally instead of stalling the caller.
fn parse_inline(text: &str) -> Vec<StyledSpan> {
    let mut spans = Vec::new();
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if let Some((next, span)) = parse_bold(text, i).or_else(|| parse_italic(text, i))
            && next > i
        {
            spans.push(span);
            i = next;
            continue;
        }
        if let Some((next, span)) = parse_link(text, i).or_else(|| parse_code(text, i))
            && next > i
        {
            spans.push(span);
            i = next;
            continue;
        }

        // No span opened here. Consume the plain-text run up to the next markup starter;
        // when the current byte is itself an opener that failed to match, consume it as a
        // literal so the loop always advances.
        let start = i;
        i = bytes[start..]
            .iter()
            .position(|&b| matches!(b, b'*' | b'[' | b'`'))
            .map_or(len, |k| start + k)
            .max(start + 1);
        spans.push(StyledSpan {
            text: text[start..i].to_string(),
            style: SpanStyle::NONE,
        });
    }

    spans
}

/// Find the first unescaped occurrence of `marker` at or after `start`, as a byte offset
/// into `text`. A marker preceded by `\` is skipped and the search continues past it.
fn find_inline_end(text: &str, start: usize, marker: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let marker_bytes = marker.as_bytes();
    bytes[start..]
        .windows(marker_bytes.len())
        .enumerate()
        .position(|(k, window)| {
            window == marker_bytes && {
                let at = k + start;
                at == 0 || bytes[at - 1] != b'\\'
            }
        })
        .map(|k| k + start)
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
#[path = "lib_tests.rs"]
mod tests;
