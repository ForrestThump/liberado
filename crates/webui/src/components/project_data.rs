//! Coding projects from `GET /api/projects`.
//!
//! Selecting a row remembers it for this window and opens the chat. The daemon does not
//! accept a project on a chat message. A project is only a field on a goal
//! (`POST /api/goals`, `payload.project`). The chip and the projects page say so.

use dioxus::prelude::*;
use serde::Deserialize;

/// Shown on the projects page. Projects are config, not a create form.
pub(crate) const PROJECT_SOURCE: &str =
    "Projects are declared in topology.toml. This list reads them. It does not create one.";

/// Shown on the projects page. A phone has no hover, so the gap is not only a tooltip.
pub(crate) const PROJECT_BIND_GAP: &str = "Choosing a project remembers it for this window and opens the chat. \
     A chat message cannot carry a project. The daemon accepts one only as payload.project on POST /api/goals.";

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

pub(crate) fn project_chip_line(name: &str) -> String {
    format!("Project: {name}. Not attached to this chat.")
}

/// One line above the composer when a project is selected. Empty when none is.
#[component]
pub(crate) fn ProjectContextChip(active_project: Signal<Option<String>>) -> Element {
    let Some(name) = active_project() else {
        return rsx! {};
    };
    let line = project_chip_line(&name);
    rsx! {
        p {
            class: "composer-project",
            title: PROJECT_BIND_GAP,
            "{line}"
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
    fn the_gap_names_the_goal_endpoint_and_not_a_chat_field() {
        assert!(PROJECT_SOURCE.contains("topology.toml"));
        assert!(PROJECT_SOURCE.contains("does not create"));
        assert!(PROJECT_BIND_GAP.contains("POST /api/goals"));
        assert!(PROJECT_BIND_GAP.contains("payload.project"));
        assert!(!PROJECT_BIND_GAP.contains("/api/chat"));
        let line = project_chip_line("liberado");
        assert!(line.contains("liberado"));
        assert!(line.contains("Not attached"));
    }

    #[test]
    fn dots_cycle_the_three_sidebar_colors() {
        assert_eq!(project_dot(0), "var(--dot-cyan)");
        assert_eq!(project_dot(1), "var(--dot-green)");
        assert_eq!(project_dot(2), "var(--dot-orange)");
        assert_eq!(project_dot(3), "var(--dot-cyan)");
    }
}
