use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct OutboxDetail {
    pub title: String,
    pub body: String,
    pub kind: String,
    pub due_at_local: String,
    pub late: bool,
    pub catch_up: bool,
    pub source: Option<serde_json::Value>,
    pub obsidian_url: Option<String>,
}
