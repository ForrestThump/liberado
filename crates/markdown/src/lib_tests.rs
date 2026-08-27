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

fn paragraph_spans(input: &str) -> Vec<StyledSpan> {
    match markdown_to_lines(input).as_slice() {
        [MarkdownLine::Paragraph(spans)] => spans.clone(),
        other => panic!("expected single Paragraph, got {other:?}"),
    }
}

#[test]
fn unterminated_star_renders_literally() {
    let spans = paragraph_spans("a * b");
    assert_eq!(
        spans,
        vec![
            StyledSpan {
                text: "a ".into(),
                style: SpanStyle::NONE
            },
            StyledSpan {
                text: "*".into(),
                style: SpanStyle::NONE
            },
            StyledSpan {
                text: " b".into(),
                style: SpanStyle::NONE
            },
        ]
    );
}

#[test]
fn lone_markup_characters_render_literally() {
    for input in ["*", "[", "`"] {
        let spans = paragraph_spans(input);
        assert_eq!(spans.len(), 1, "input {input:?}");
        assert_eq!(spans[0].text, input);
        assert_eq!(spans[0].style, SpanStyle::NONE);
    }
}

#[test]
fn unterminated_code_span_is_literal() {
    let spans = paragraph_spans("use `foo");
    assert_eq!(
        spans.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["use ", "`", "foo"]
    );
}

#[test]
fn bracket_without_paren_is_literal() {
    let spans = paragraph_spans("see [docs]( url");
    assert_eq!(
        spans.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["see ", "[", "docs]( url"]
    );
}

#[test]
fn unclosed_bracket_is_literal() {
    let spans = paragraph_spans("see [docs");
    assert_eq!(
        spans.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["see ", "[", "docs"]
    );
}

#[test]
fn trailing_bracket_without_paren_is_literal() {
    let spans = paragraph_spans("see [docs]");
    assert_eq!(
        spans.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["see ", "[", "docs]"]
    );
}

#[test]
fn bold_at_line_end_exact() {
    let spans = paragraph_spans("hi **b**");
    assert_eq!(
        spans,
        vec![
            StyledSpan {
                text: "hi ".into(),
                style: SpanStyle::NONE
            },
            StyledSpan {
                text: "b".into(),
                style: SpanStyle::BOLD
            },
        ]
    );
}

#[test]
fn bold_then_tail_exact() {
    let spans = paragraph_spans("**b** tail");
    assert_eq!(
        spans
            .iter()
            .map(|s| (s.text.as_str(), s.style))
            .collect::<Vec<_>>(),
        vec![("b", SpanStyle::BOLD), (" tail", SpanStyle::NONE)]
    );
}

#[test]
fn link_at_line_end_exact() {
    let spans = paragraph_spans("see [d](u)");
    assert_eq!(
        spans
            .iter()
            .map(|s| (s.text.as_str(), s.style))
            .collect::<Vec<_>>(),
        vec![("see ", SpanStyle::NONE), ("d", SpanStyle::LINK)]
    );
}

#[test]
fn minimal_link_exact() {
    let spans = paragraph_spans("[a](b)");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].text, "a");
    assert_eq!(spans[0].style, SpanStyle::LINK);
}

#[test]
fn code_span_boundaries_exact() {
    let leading = paragraph_spans("`c` x");
    assert_eq!(
        leading
            .iter()
            .map(|s| (s.text.as_str(), s.style))
            .collect::<Vec<_>>(),
        vec![("c", SpanStyle::CODE), (" x", SpanStyle::NONE)]
    );
    let trailing = paragraph_spans("x `c`");
    assert_eq!(
        trailing
            .iter()
            .map(|s| (s.text.as_str(), s.style))
            .collect::<Vec<_>>(),
        vec![("x ", SpanStyle::NONE), ("c", SpanStyle::CODE)]
    );
}

#[test]
fn italic_at_line_start_exact() {
    let spans = paragraph_spans("*i* x");
    assert_eq!(
        spans
            .iter()
            .map(|s| (s.text.as_str(), s.style))
            .collect::<Vec<_>>(),
        vec![("i", SpanStyle::ITALIC), (" x", SpanStyle::NONE)]
    );
}

#[test]
fn double_star_without_closer_falls_back_to_literals() {
    let spans = paragraph_spans("a **b");
    assert_eq!(
        spans.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["a ", "*", "*", "b"]
    );
}

#[test]
fn italic_never_opens_at_second_star_of_a_pair() {
    let spans = paragraph_spans("*a**b*c");
    assert_eq!(
        spans
            .iter()
            .map(|s| (s.text.as_str(), s.style))
            .collect::<Vec<_>>(),
        vec![
            ("a", SpanStyle::ITALIC),
            ("*", SpanStyle::NONE),
            ("b", SpanStyle::NONE),
            ("*", SpanStyle::NONE),
            ("c", SpanStyle::NONE),
        ]
    );
}

#[test]
fn horizontal_rule_forms() {
    for input in ["---", "***", "___"] {
        match markdown_to_lines(input).as_slice() {
            [MarkdownLine::HorizontalRule] => {}
            other => panic!("input {input:?}: expected HorizontalRule, got {other:?}"),
        }
    }
}

#[test]
fn nested_brackets_in_link_text() {
    let spans = paragraph_spans("[a [b] c](u)");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].text, "a [b] c");
    assert_eq!(spans[0].style, SpanStyle::LINK);
}

#[test]
fn nested_parens_in_link_url() {
    let spans = paragraph_spans("[t](h(a)b)");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].text, "t");
    assert_eq!(spans[0].style, SpanStyle::LINK);
}

#[test]
fn escaped_stars_render_as_text() {
    let spans = paragraph_spans("\\*lit\\*");
    assert_eq!(
        spans.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["\\", "*", "lit\\", "*"]
    );
}

#[test]
fn literal_star_then_valid_link_resumes_parsing() {
    let spans = paragraph_spans("a * [b](c)");
    assert_eq!(
        spans
            .iter()
            .map(|s| (s.text.as_str(), s.style))
            .collect::<Vec<_>>(),
        vec![
            ("a ", SpanStyle::NONE),
            ("*", SpanStyle::NONE),
            (" ", SpanStyle::NONE),
            ("b", SpanStyle::LINK),
        ]
    );
}

#[test]
fn lone_star_before_later_bold_stays_literal() {
    let spans = paragraph_spans("x * y **z**");
    assert_eq!(
        spans
            .iter()
            .map(|s| (s.text.as_str(), s.style))
            .collect::<Vec<_>>(),
        vec![
            ("x ", SpanStyle::NONE),
            (" y ", SpanStyle::ITALIC),
            ("*", SpanStyle::NONE),
            ("z", SpanStyle::NONE),
            ("*", SpanStyle::NONE),
            ("*", SpanStyle::NONE),
        ]
    );
}

#[test]
fn empty_markup_spans_render_literally() {
    let cases = [
        ("**", vec!["*", "*"]),
        ("****", vec!["*", "*", "*", "*"]),
        ("``", vec!["`", "`"]),
        ("[]", vec!["[", "]"]),
        ("[]()", vec!["[", "]()"]),
    ];
    for (input, expected) in cases {
        assert_eq!(
            paragraph_spans(input)
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>(),
            expected,
            "input {input:?}"
        );
    }
}

#[test]
fn empty_input_yields_no_lines() {
    assert!(markdown_to_lines("").is_empty());
}
