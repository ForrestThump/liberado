//! User-project writes and the `/projects` path.
//!
//! Kept beside `project_data` so the list, the chip, and the patch helpers stay
//! in the file that already owned them.

use std::collections::HashMap;

use serde::Deserialize;

use super::project_data::{ProjectChatRow, ProjectRow, normalize_row};

/// A browser path under `/projects`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProjectPath {
    List,
    Detail(String),
}

pub(crate) async fn create_user_project(
    api_base: &str,
    body: &serde_json::Value,
) -> Result<ProjectRow, String> {
    let url = format!("{api_base}/api/chat-projects");
    let resp = crate::http::post_json(&url, body)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    if !status.is_success() {
        return Err(api_error(&text, status.as_u16()));
    }
    parse_one_project(&text)
}

pub(crate) async fn update_user_project(
    api_base: &str,
    id: &str,
    body: &serde_json::Value,
) -> Result<ProjectRow, String> {
    let url = format!("{api_base}/api/chat-projects/{}", path_segment(id));
    let resp = crate::http::patch_json(&url, body)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    if !status.is_success() {
        return Err(api_error(&text, status.as_u16()));
    }
    parse_one_project(&text)
}

pub(crate) async fn delete_user_project(api_base: &str, id: &str) -> Result<(), String> {
    let url = format!("{api_base}/api/chat-projects/{}", path_segment(id));
    let resp = crate::http::delete(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        Err(api_error(&text, status))
    }
}

pub(crate) async fn fetch_project_chats(
    api_base: &str,
    id: &str,
) -> Result<Vec<ProjectChatRow>, String> {
    let url = format!("{api_base}/api/chat-projects/{}/chats", path_segment(id));
    let resp = crate::http::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    if !status.is_success() {
        return Err(api_error(&text, status.as_u16()));
    }
    parse_project_chats(&text)
}

pub(crate) async fn create_project_chat(api_base: &str, project: &str) -> Result<String, String> {
    let url = format!("{api_base}/api/conversations");
    let resp = crate::http::post_json(&url, &serde_json::json!({ "project": project }))
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    if !status.is_success() {
        return Err(api_error(&text, status.as_u16()));
    }
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| {
            value
                .get("id")
                .and_then(|id| id.as_str())
                .map(str::to_string)
        })
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "Chat was created without an id".into())
}

fn parse_one_project(body: &str) -> Result<ProjectRow, String> {
    let row: ProjectRow = serde_json::from_str(body).map_err(|e| format!("Bad response: {e}"))?;
    Ok(normalize_row(row))
}

fn parse_project_chats(body: &str) -> Result<Vec<ProjectChatRow>, String> {
    #[derive(Deserialize)]
    struct ChatsResponse {
        #[serde(default)]
        chats: Vec<ProjectChatRow>,
    }
    let parsed: ChatsResponse =
        serde_json::from_str(body).map_err(|e| format!("Bad response: {e}"))?;
    Ok(parsed.chats)
}

fn api_error(body: &str, status: u16) -> String {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|err| err.as_str())
                .map(str::to_string)
        })
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| body.trim().to_string());
    if message.is_empty() {
        format!("Request failed ({status})")
    } else {
        format!("{message} ({status})")
    }
}

pub(crate) fn project_route_from_path(path: &str) -> Option<ProjectPath> {
    let trimmed = path.trim().trim_end_matches('/');
    if trimmed == "/projects" {
        return Some(ProjectPath::List);
    }
    let rest = trimmed.strip_prefix("/projects/")?;
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    let id = percent_decode(rest);
    if id.is_empty() {
        None
    } else {
        Some(ProjectPath::Detail(id))
    }
}

pub(crate) fn project_path(id: Option<&str>) -> String {
    match id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => format!("/projects/{}", path_segment(id)),
        None => "/projects".into(),
    }
}

pub(crate) fn path_segment(id: &str) -> String {
    let mut out = String::new();
    for byte in id.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            ) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn project_editable(source: &str) -> bool {
    source == "user"
}

pub(crate) fn delete_prompt(name: &str) -> String {
    format!("Delete {name}? Chats stay in the chat list. They are detached from this project.")
}

pub(crate) fn new_project_body(name: &str, instructions: &str) -> Option<serde_json::Value> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let instructions = instructions.trim();
    if instructions.is_empty() {
        Some(serde_json::json!({ "name": name }))
    } else {
        Some(serde_json::json!({ "name": name, "instructions": instructions }))
    }
}

pub(crate) fn chat_date(updated_at: &str) -> String {
    let trimmed = updated_at.trim();
    if trimmed.len() >= 10 && trimmed.as_bytes()[4] == b'-' && trimmed.as_bytes()[7] == b'-' {
        trimmed[..10].to_string()
    } else {
        trimmed.to_string()
    }
}

pub(crate) fn label_map(rows: &[ProjectRow]) -> HashMap<String, String> {
    rows.iter()
        .map(|row| (row.id.clone(), row.name.clone()))
        .collect()
}

pub(crate) fn display_name(
    key: &str,
    loaded: Option<&Result<HashMap<String, String>, String>>,
) -> String {
    loaded
        .and_then(|result| result.as_ref().ok())
        .and_then(|map| map.get(key).cloned())
        .unwrap_or_else(|| key.to_string())
}
