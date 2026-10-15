use super::{escape_html, markdown_to_html};

#[test]
fn escape_replaces_markup_characters() {
    assert_eq!(escape_html(r#"a<b>&"c""#), "a&lt;b&gt;&amp;&quot;c&quot;");
}

#[test]
fn paragraph_styles_and_a_script_stay_text() {
    let html = markdown_to_html("**b** *i* `c` <script>");
    assert!(html.contains("<strong>b</strong>"));
    assert!(html.contains("<em>i</em>"));
    assert!(html.contains("<code>c</code>"));
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains("<script>"));
}

#[test]
fn blocks_use_their_tags() {
    let html = markdown_to_html("# Title\n\n- one\n- two\n\n```\n<x>\n```\n\n---");
    assert!(html.contains("<h1>Title</h1>"));
    assert!(html.contains("<ul><li>one</li><li>two</li></ul>"));
    assert!(html.contains("<pre><code>&lt;x&gt;</code></pre>"));
    assert!(html.contains("<hr>"));
}

#[test]
fn empty_input_is_empty_html() {
    assert_eq!(markdown_to_html(""), "");
}
