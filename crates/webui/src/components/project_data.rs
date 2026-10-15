//! Coding projects from `GET /api/projects`.
//!
//! A pick attaches the project to the open chat with `PATCH /api/conversations/{id}`.
//! A chat that does not exist yet keeps the name until the first message, which sends
//! it on that same patch as the title.

use dioxus::prelude::*;
use serde::Deserialize;

/// Shown on the projects page. A phone has no hover, so the sentence is not only a tooltip.
pub(crate) const PROJECT_BIND_GAP: &str = "Choosing a project attaches it to the open chat. \
     A new chat keeps the choice until the first message saves it on the conversation.";

const DOTS: [&str; 3] = ["var(--dot-cyan)", "var(--dot-green)", "var(--dot-orange)"];

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct ProjectRow {
    pub name: String,
    pub root: String,
    #[serde(default)]
    pub write_class: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProjectsResponse {
    #[serde(default)]
    projects: Vec<ProjectRow>,
}

pub(crate) fn parse_projects(body: &str) -> Result<Vec<ProjectRow>, String> {
    let parsed: ProjectsResponse =
        serde_json::from_str(body).map_err(|e| format!("Bad response: {e}"))?;
    Ok(parsed.projects)
}

pub(crate) async fn fetch_projects(api_base: &str) -> Result<Vec<ProjectRow>, String> {
    let url = format!("{api_base}/api/projects");
    let resp = reqwest::get(&url)
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
pub(crate) fn close_phone_drawer(mut collapsed: Signal<bool>) {
    collapsed.set(collapsed_after_pick(
        crate::is_narrow_viewport(),
        collapsed(),
    ));
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
    let resp = reqwest::Client::new()
        .patch(&url)
        .json(&project_patch_body(project))
        .send()
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
                Ok(()) => list_epoch.set(list_epoch() + 1),
                Err(error) => {
                    active_project.set(previous);
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
    let Some(name) = active_project() else {
        return rsx! {};
    };
    let saved = session().is_some();
    let line = project_chip_line(&name, saved);
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
        assert_eq!(rows[0].root, "/src");
        assert_eq!(rows[0].write_class.as_deref(), Some("agent_writable"));
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
