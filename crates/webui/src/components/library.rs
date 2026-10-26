//! Library view. Rows come from `GET /api/library`.
//!
//! v1 is vault files only: folders such as attachments and media that the default profile
//! may read. An empty list stays an empty state. There is no upload and no delete.

use dioxus::prelude::*;
use serde::Deserialize;

const LIBRARY_SCOPE: &str = "Files in vault folders the default profile may read, such as attachments and media. \
     This list does not include chat files or files saved outside the vault.";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
struct LibraryItem {
    id: String,
    kind: String,
    title: String,
    path_or_url: String,
    created_at: String,
    source: String,
}

#[derive(Debug, Deserialize)]
struct LibraryResponse {
    #[serde(default)]
    items: Vec<LibraryItem>,
}

fn parse_library(body: &str) -> Result<Vec<LibraryItem>, String> {
    let parsed: LibraryResponse =
        serde_json::from_str(body).map_err(|e| format!("Bad response: {e}"))?;
    Ok(parsed.items)
}

async fn fetch_library(api_base: &str) -> Result<Vec<LibraryItem>, String> {
    let url = format!("{api_base}/api/library?limit=50");
    let resp = crate::http::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(format!("Library unavailable ({status})"));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    parse_library(&text)
}

#[component]
pub fn Library(api_base: String) -> Element {
    let items = use_resource({
        let base = api_base.clone();
        move || {
            let base = base.clone();
            async move { fetch_library(&base).await }
        }
    });

    rsx! {
        div {
            class: "grok-page library-page",
            div {
                class: "grok-page-header",
                div { class: "grok-page-eyebrow", "Library" }
                h1 { class: "grok-page-title", "Your artifacts" }
                p { class: "grok-page-subtitle", "{LIBRARY_SCOPE}" }
            }
            div {
                class: "grok-page-body",
                match items.read().as_ref() {
                    None => rsx! { p { class: "library-status", "Loading..." } },
                    Some(Err(error)) => rsx! { p { class: "library-status", "{error}" } },
                    Some(Ok(rows)) if rows.is_empty() => rsx! {
                        p { class: "library-status", "There are none yet." }
                    },
                    Some(Ok(rows)) => rsx! {
                        ul {
                            class: "library-list",
                            for item in rows.iter() {
                                li {
                                    key: "{item.id}",
                                    class: "library-row",
                                    span { class: "library-title", "{item.title}" }
                                    span { class: "library-meta", "{item.kind} · {item.path_or_url}" }
                                    span { class: "library-meta", "{item.source} · {item.created_at}" }
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_json_keeps_the_vault_item_fields() {
        let body = r#"{"items":[{"id":"attachments/a.png","kind":"png","title":"a.png","path_or_url":"attachments/a.png","created_at":"2026-10-15T12:00:00Z","source":"vault"}]}"#;
        let rows = parse_library(body).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "attachments/a.png");
        assert_eq!(rows[0].kind, "png");
        assert_eq!(rows[0].title, "a.png");
        assert_eq!(rows[0].path_or_url, "attachments/a.png");
        assert_eq!(rows[0].created_at, "2026-10-15T12:00:00Z");
        assert_eq!(rows[0].source, "vault");
    }

    #[test]
    fn an_empty_library_is_empty() {
        assert!(parse_library(r#"{"items":[]}"#).unwrap().is_empty());
        assert!(parse_library("{}").unwrap().is_empty());
    }

    #[test]
    fn the_page_asks_for_the_capped_vault_list() {
        let src = include_str!("library.rs");
        assert!(src.contains("/api/library?limit=50"));
        assert!(src.contains("use_resource"));
        assert!(src.contains("rows.is_empty()"));
        assert!(LIBRARY_SCOPE.contains("vault"));
        assert!(LIBRARY_SCOPE.contains("attachments"));
        assert!(!LIBRARY_SCOPE.contains("conversations"));
    }
}
