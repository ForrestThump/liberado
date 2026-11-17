//! Chat projects from `GET /api/chat-projects`.
//!
//! Topology rows keep a coding root. A project created here has none.
//! Attaching a project still uses `PATCH /api/conversations/{id}`.

use std::collections::HashMap;

use dioxus::prelude::*;
use serde::Deserialize;

/// Shown on the projects page.
pub(crate) const PROJECT_SOURCE: &str = "Topology projects stay in topology.toml and keep a \
     coding root. New project creates a chat project with no coding root.";

/// Shown on the projects page. A phone has no hover, so the sentence is not only a tooltip.
pub(crate) const PROJECT_BIND_GAP: &str = "Choosing a project attaches it to the open chat. \
     A new chat keeps the choice until the first message saves it on the conversation.";

const DOTS: [&str; 3] = ["var(--dot-cyan)", "var(--dot-green)", "var(--dot-orange)"];

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct ProjectRow {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub write_class: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct ProjectChatRow {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
struct ProjectsResponse {
    #[serde(default)]
    projects: Vec<ProjectRow>,
}

pub(crate) fn parse_projects(body: &str) -> Result<Vec<ProjectRow>, String> {
    let parsed: ProjectsResponse =
        serde_json::from_str(body).map_err(|e| format!("Bad response: {e}"))?;
    Ok(parsed.projects.into_iter().map(normalize_row).collect())
}

fn normalize_row(mut row: ProjectRow) -> ProjectRow {
    if row.id.trim().is_empty() {
        row.id = row.name.clone();
    }
    if row.source.trim().is_empty() {
        row.source = if row.root.is_some() {
            "topology".into()
        } else {
            "user".into()
        };
    }
    row
}

pub(crate) async fn fetch_projects(api_base: &str) -> Result<Vec<ProjectRow>, String> {
    let url = format!("{api_base}/api/chat-projects");
    let resp = crate::http::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(format!("Projects unavailable ({status})"));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    parse_projects(&text)
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

pub(crate) fn parse_project_chats(body: &str) -> Result<Vec<ProjectChatRow>, String> {
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

pub(crate) fn project_dot(index: usize) -> &'static str {
    DOTS[index % DOTS.len()]
}

/// Empty names are not a selection.
pub(crate) fn selected_project_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Phone drawer closes after a pick. A desk panel stays as it was.
pub(crate) fn collapsed_after_pick(narrow: bool, collapsed: bool) -> bool {
    narrow || collapsed
}

/// Close the phone drawer after a nav item opens a view. A desk rail is unchanged.
pub(crate) fn replace_project_path(path: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(window) = web_sys::window() {
            if let Ok(history) = window.history() {
                let _ =
                    history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(path));
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = path;
    }
}

pub(crate) fn close_phone_drawer(mut collapsed: Signal<bool>) {
    let Ok(now) = collapsed.try_peek().map(|value| *value) else {
        return;
    };
    let _ = crate::signal_alive::set_if_alive(
        &mut collapsed,
        collapsed_after_pick(crate::is_narrow_viewport(), now),
    );
}

/// A browser path under `/projects`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProjectPath {
    List,
    Detail(String),
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

pub(crate) fn project_chip_line(name: &str, saved_on_chat: bool) -> String {
    if saved_on_chat {
        format!("Project: {name}")
    } else {
        format!("Project: {name}. Saved on this chat with the first message.")
    }
}

pub(crate) fn project_patch_body(project: Option<&str>) -> serde_json::Value {
    match trimmed_project(project) {
        Some(name) => serde_json::json!({ "project": name }),
        None => serde_json::json!({ "project": serde_json::Value::Null }),
    }
}

/// Title patch used after the first message. Includes the project only when one is chosen,
/// so a chat with no project is not cleared.
pub(crate) fn title_patch_body(title: &str, project: Option<&str>) -> serde_json::Value {
    match trimmed_project(project) {
        Some(name) => serde_json::json!({ "title": title, "project": name }),
        None => serde_json::json!({ "title": title }),
    }
}

fn trimmed_project(project: Option<&str>) -> Option<&str> {
    project.map(str::trim).filter(|name| !name.is_empty())
}

pub(crate) async fn patch_project(
    api_base: &str,
    id: &str,
    project: Option<&str>,
) -> Result<(), String> {
    let url = format!("{api_base}/api/conversations/{id}");
    let resp = crate::http::patch_json(&url, &project_patch_body(project))
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Err(format!("Project was not saved ({status}): {text}"))
    }
}

/// Chats whose stored project is `name`.
pub(crate) fn chats_for_project<'a>(
    chats: &'a [chat_client_contract::ConvHeader],
    name: &str,
) -> Vec<&'a chat_client_contract::ConvHeader> {
    chats
        .iter()
        .filter(|chat| chat.project.as_deref() == Some(name))
        .collect()
}

pub(crate) fn queue_project_patch(
    api_base: String,
    id: String,
    project: Option<String>,
    list_epoch: Signal<u64>,
    active_project: Signal<Option<String>>,
    previous: Option<String>,
) {
    #[cfg(target_arch = "wasm32")]
    {
        let mut list_epoch = list_epoch;
        let mut active_project = active_project;
        wasm_bindgen_futures::spawn_local(async move {
            match patch_project(&api_base, &id, project.as_deref()).await {
                Ok(()) => {
                    let _ = crate::signal_alive::with_mut_if_alive(&mut list_epoch, |epoch| {
                        *epoch += 1;
                    });
                }
                Err(error) => {
                    let _ = crate::signal_alive::set_if_alive(&mut active_project, previous);
                    web_sys::console::log_1(&error.into());
                }
            }
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (api_base, id, project, list_epoch, active_project, previous);
    }
}

/// One line above the composer when a project is selected. Empty when none is.
#[component]
pub(crate) fn ProjectContextChip(
    api_base: String,
    mut active_project: Signal<Option<String>>,
    session: Signal<Option<String>>,
    list_epoch: Signal<u64>,
) -> Element {
    let labels = use_resource({
        let base = api_base.clone();
        move || {
            let base = base.clone();
            async move { fetch_projects(&base).await.map(|rows| label_map(&rows)) }
        }
    });
    let Some(name) = active_project() else {
        return rsx! {};
    };
    let loaded = labels.read();
    let shown = display_name(&name, loaded.as_ref());
    let saved = session().is_some();
    let line = project_chip_line(&shown, saved);
    rsx! {
        p {
            class: "composer-project",
            title: PROJECT_BIND_GAP,
            "{line}"
            if saved {
                button {
                    class: "composer-project-detach",
                    r#type: "button",
                    onclick: move |_| {
                        let id = session().unwrap_or_default();
                        if id.is_empty() {
                            active_project.set(None);
                            return;
                        }
                        let previous = active_project();
                        active_project.set(None);
                        queue_project_patch(
                            api_base.clone(),
                            id,
                            None,
                            list_epoch,
                            active_project,
                            previous,
                        );
                    },
                    "Detach"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_json_keeps_name_root_and_write_class() {
        let body =
            r#"{"projects":[{"name":"liberado","root":"/src","write_class":"agent_writable"}]}"#;
        let rows = parse_projects(body).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "liberado");
        assert_eq!(rows[0].id, "liberado");
        assert_eq!(rows[0].source, "topology");
        assert_eq!(rows[0].root.as_deref(), Some("/src"));
        assert_eq!(rows[0].write_class.as_deref(), Some("agent_writable"));
    }

    #[test]
    fn a_user_project_has_no_coding_root() {
        let body =
            r#"{"projects":[{"id":"01P","name":"Trip","source":"user","instructions":"Pack"}]}"#;
        let rows = parse_projects(body).unwrap();
        assert_eq!(rows[0].id, "01P");
        assert_eq!(rows[0].source, "user");
        assert!(rows[0].root.is_none());
        assert!(rows[0].write_class.is_none());
        assert!(project_editable(&rows[0].source));
        assert!(!project_editable("topology"));
    }

    #[test]
    fn an_empty_project_list_is_empty() {
        assert!(parse_projects(r#"{"projects":[]}"#).unwrap().is_empty());
    }

    #[test]
    fn a_blank_name_is_not_a_selection() {
        assert_eq!(
            selected_project_name("  liberado "),
            Some("liberado".into())
        );
        assert_eq!(selected_project_name("   "), None);
    }

    #[test]
    fn a_phone_pick_closes_the_drawer_and_a_desk_pick_does_not() {
        assert!(collapsed_after_pick(true, false));
        assert!(collapsed_after_pick(true, true));
        assert!(!collapsed_after_pick(false, false));
        assert!(collapsed_after_pick(false, true));
    }

    #[test]
    fn close_phone_drawer_uses_the_same_rule_as_a_project_pick() {
        let src = include_str!("project_data.rs");
        let body = src
            .split("fn close_phone_drawer")
            .nth(1)
            .expect("close_phone_drawer");
        let body = body.split("pub(crate) fn").next().unwrap_or(body);
        assert!(body.contains("collapsed_after_pick"));
        assert!(body.contains("is_narrow_viewport"));
    }

    #[test]
    fn the_projects_page_names_topology_and_not_a_create_form() {
        assert!(PROJECT_SOURCE.contains("topology.toml"));
        assert!(PROJECT_SOURCE.contains("New project"));
        assert!(PROJECT_SOURCE.contains("no coding root"));
    }

    #[test]
    fn paths_round_trip_and_a_blank_name_is_not_a_body() {
        assert_eq!(
            project_route_from_path("/projects"),
            Some(ProjectPath::List)
        );
        assert_eq!(
            project_route_from_path("/projects/01P"),
            Some(ProjectPath::Detail("01P".into()))
        );
        assert_eq!(
            project_route_from_path("/projects/a%20b"),
            Some(ProjectPath::Detail("a b".into()))
        );
        assert_eq!(project_route_from_path("/other"), None);
        assert_eq!(project_path(Some("a b")), "/projects/a%20b");
        assert_eq!(project_path(None), "/projects");
        assert!(new_project_body("  ", "notes").is_none());
        assert_eq!(
            new_project_body(" Trip ", " Pack "),
            Some(serde_json::json!({ "name": "Trip", "instructions": "Pack" }))
        );
        assert_eq!(chat_date("2026-11-01T12:00:00Z"), "2026-11-01");
        let prompt = delete_prompt("Trip");
        assert!(prompt.contains("Trip"));
        assert!(prompt.contains("detached"));
        assert!(prompt.contains("chat list"));
    }

    #[test]
    fn the_chip_says_when_the_project_is_saved_on_the_chat() {
        assert!(PROJECT_BIND_GAP.contains("attaches"));
        assert!(PROJECT_BIND_GAP.contains("first message"));
        let waiting = project_chip_line("liberado", false);
        assert!(waiting.contains("liberado"));
        assert!(waiting.contains("first message"));
        assert_eq!(project_chip_line("liberado", true), "Project: liberado");
    }

    #[test]
    fn patch_bodies_set_and_clear_without_dropping_the_title() {
        assert_eq!(
            project_patch_body(Some("liberado")),
            serde_json::json!({ "project": "liberado" })
        );
        assert_eq!(
            project_patch_body(None),
            serde_json::json!({ "project": null })
        );
        assert_eq!(
            title_patch_body("Notes", None),
            serde_json::json!({ "title": "Notes" })
        );
        assert_eq!(
            title_patch_body("Notes", Some("liberado")),
            serde_json::json!({ "title": "Notes", "project": "liberado" })
        );
    }

    #[test]
    fn chats_group_under_their_project() {
        let rows = vec![
            chat_client_contract::ConvHeader {
                id: "a".into(),
                project: Some("liberado".into()),
                ..Default::default()
            },
            chat_client_contract::ConvHeader {
                id: "b".into(),
                project: Some("other".into()),
                ..Default::default()
            },
            chat_client_contract::ConvHeader {
                id: "c".into(),
                ..Default::default()
            },
        ];
        let grouped = chats_for_project(&rows, "liberado");
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].id, "a");
    }

    #[test]
    fn dots_cycle_the_three_sidebar_colors() {
        assert_eq!(project_dot(0), "var(--dot-cyan)");
        assert_eq!(project_dot(1), "var(--dot-green)");
        assert_eq!(project_dot(2), "var(--dot-orange)");
        assert_eq!(project_dot(3), "var(--dot-cyan)");
    }
}
