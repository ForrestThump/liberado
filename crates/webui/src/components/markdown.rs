use dioxus::prelude::*;

/// Markdown to HTML, **with trailing whitespace removed**.
///
/// `push_html` ends its output with a newline after the last closing tag. Injected as
/// `dangerous_inner_html`, that newline survives as a text node — and a text node after a block
/// element is anonymous inline content, so it lays out as a whole extra line box. A one-line message
/// measured 44.8px inside a container whose paragraph was 22.4px: the bubble was exactly twice as
/// tall as its content, with the empty line sitting under every message.
///
/// This is not reachable from CSS. `.markdown-body p:last-child { margin-bottom: 0 }` was already
/// set and correct; it simply cannot address a node that is not an element. The newline has to stop
/// being emitted.
///
/// Trimming the whole string is safe for code blocks: the newline `<pre>` content needs is *inside*
/// the element, before `</pre>`, so only inter-block whitespace is ever removed here.
pub fn render_markdown(text: &str) -> String {
    let parser = pulldown_cmark::Parser::new(text);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    html.truncate(html.trim_end().len());
    html
}

#[component]
pub fn MarkdownText(content: String) -> Element {
    let html = render_markdown(&content);
    rsx! {
        div {
            class: "markdown-body",
            dangerous_inner_html: "{html}",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::render_markdown;

    /// The defect this function's trim exists for: a trailing newline becomes a text node in the
    /// DOM, and a text node after a block element lays out as a full extra line. Every message
    /// bubble carried one, which on a one-line message doubled its height.
    #[test]
    fn output_ends_at_the_last_tag() {
        let html = render_markdown("Full list");
        assert_eq!(html, "<p>Full list</p>");
        assert!(
            !html.ends_with('\n'),
            "a trailing newline renders as an empty line under the message"
        );
    }

    /// Multi-block content must still be separated — the trim takes the *trailing* newline, not the
    /// ones between blocks.
    #[test]
    fn interior_structure_is_untouched() {
        let html = render_markdown("first\n\nsecond");
        assert_eq!(html, "<p>first</p>\n<p>second</p>");
    }

    /// A code block's own final newline lives inside `<pre>`, before the closing tag, so trimming
    /// the string cannot reach it. Losing it would silently drop the last line break of pasted code.
    #[test]
    fn code_block_keeps_its_internal_newline() {
        let html = render_markdown("```\nlet x = 1;\n```");
        assert!(
            html.contains("let x = 1;\n"),
            "the newline inside the code block must survive: {html}"
        );
        assert!(
            html.ends_with("</pre>"),
            "but the one after it must not: {html}"
        );
    }

    /// Empty input has no last tag to end at, and must not panic or invent one.
    #[test]
    fn empty_input_is_empty_output() {
        assert_eq!(render_markdown(""), "");
        assert_eq!(render_markdown("   \n  "), "");
    }
}
