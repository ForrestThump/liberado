use dioxus::prelude::*;

pub fn render_markdown(text: &str) -> String {
    let parser = pulldown_cmark::Parser::new(text);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
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
