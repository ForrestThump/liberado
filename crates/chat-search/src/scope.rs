//! Project filter for chat search.
//!
//! An explicit `project` key wins. `scope: "project"` means the project stored on
//! the current chat. Omitting both searches every chat.

use std::path::Path;

use liberado_session_store::Record;

/// What the current chat's log says about its project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurrentProject {
    /// The conversation file is missing or has no header.
    UnknownChat,
    /// The chat exists and is not attached to a project.
    Unscoped,
    /// The stored project key (topology name or user project id).
    Project(String),
}

/// Last header in `root/{id}.jsonl`. Later header lines win, same as store replay.
pub fn stored_project(root: &Path, conversation_id: &str) -> CurrentProject {
    let id = conversation_id.trim();
    if id.is_empty() {
        return CurrentProject::UnknownChat;
    }
    let Ok(contents) = std::fs::read_to_string(root.join(format!("{id}.jsonl"))) else {
        return CurrentProject::UnknownChat;
    };
    let mut saw_header = false;
    let mut project = None;
    for line in contents.split('\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(Record::Header(header)) = serde_json::from_str::<Record>(line) else {
            continue;
        };
        saw_header = true;
        project = header.project.clone();
    }
    if !saw_header {
        return CurrentProject::UnknownChat;
    }
    match project
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
    {
        Some(name) => CurrentProject::Project(name),
        None => CurrentProject::Unscoped,
    }
}

/// The project key to pass to [`crate::search_in`], or `None` to search every chat.
pub fn resolve_project_filter(
    project: Option<&str>,
    scope: Option<&str>,
    current: Option<&CurrentProject>,
) -> Result<Option<String>, &'static str> {
    if let Some(project) = project.map(str::trim).filter(|name| !name.is_empty()) {
        return Ok(Some(project.to_string()));
    }
    match scope.map(str::trim).filter(|name| !name.is_empty()) {
        None | Some("all") => Ok(None),
        Some(scope) if scope.eq_ignore_ascii_case("project") => match current {
            Some(CurrentProject::Project(name)) => Ok(Some(name.clone())),
            Some(CurrentProject::Unscoped) => Err("this chat is not in a project"),
            Some(CurrentProject::UnknownChat) | None => {
                Err("scope project needs the current conversation")
            }
        },
        Some(_) => Err("scope must be \"project\" or \"all\""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ParsedQuery, search_in};
    use std::io::Write;

    fn write_chat(dir: &std::path::Path, id: &str, node: &str, project: Option<&str>, body: &str) {
        let project_json = match project {
            Some(project) => format!(",\"project\":\"{project}\""),
            None => String::new(),
        };
        let mut file = std::fs::File::create(dir.join(format!("{id}.jsonl"))).unwrap();
        writeln!(
            file,
            r#"{{"kind":"header","id":"{id}","title":"Chat","parent_conversation":null,"spawned_by":null,"created_at":"2026-01-01T00:00:00Z"{project_json}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"kind":"node","id":"{node}","parent_id":null,"conversation_id":"{id}","author":"user","created_at":"2026-01-02T00:00:00Z","message":{{"role":"user","content":"{body}","tool_calls":[],"tool_call_id":null}}}}"#
        )
        .unwrap();
    }

    #[test]
    fn explicit_project_wins_and_scope_project_uses_the_current_chat() {
        assert_eq!(
            resolve_project_filter(Some("alpha"), Some("project"), None).unwrap(),
            Some("alpha".into())
        );
        assert_eq!(
            resolve_project_filter(
                None,
                Some("project"),
                Some(&CurrentProject::Project("beta".into()))
            )
            .unwrap(),
            Some("beta".into())
        );
        assert_eq!(resolve_project_filter(None, None, None).unwrap(), None);
        assert_eq!(
            resolve_project_filter(None, Some("all"), Some(&CurrentProject::Unscoped)).unwrap(),
            None
        );
        assert_eq!(
            resolve_project_filter(None, Some("project"), Some(&CurrentProject::Unscoped)),
            Err("this chat is not in a project")
        );
        assert_eq!(
            resolve_project_filter(None, Some("project"), None),
            Err("scope project needs the current conversation")
        );
        assert!(resolve_project_filter(None, Some("nope"), None).is_err());
    }

    #[test]
    fn the_last_header_is_the_stored_project() {
        let dir = tempfile::tempdir().unwrap();
        let id = "01JVAAAAAAAAAAAAAAAAAAAAAA";
        let path = dir.path().join(format!("{id}.jsonl"));
        std::fs::write(
            &path,
            format!(
                r#"{{"kind":"header","id":"{id}","title":null,"created_at":"2026-01-01T00:00:00Z","project":"old"}}
{{"kind":"header","id":"{id}","title":null,"created_at":"2026-01-01T00:00:00Z","project":"new"}}
"#
            ),
        )
        .unwrap();
        assert_eq!(
            stored_project(dir.path(), id),
            CurrentProject::Project("new".into())
        );
        assert_eq!(
            stored_project(dir.path(), "missing"),
            CurrentProject::UnknownChat
        );
    }

    #[tokio::test]
    async fn a_project_filter_drops_other_chats() {
        let dir = tempfile::tempdir().unwrap();
        write_chat(
            dir.path(),
            "01JVAAAAAAAAAAAAAAAAAAAAAA",
            "01JVAAAAAAAAAAAAAAAAAAAAB1",
            Some("alpha"),
            "dark mode",
        );
        write_chat(
            dir.path(),
            "01JVAAAAAAAAAAAAAAAAAAAABB",
            "01JVAAAAAAAAAAAAAAAAAAAAB2",
            Some("beta"),
            "dark mode",
        );
        write_chat(
            dir.path(),
            "01JVAAAAAAAAAAAAAAAAAAAACC",
            "01JVAAAAAAAAAAAAAAAAAAAAB3",
            None,
            "dark mode",
        );
        let query = ParsedQuery::parse_literal("dark mode").unwrap();
        let all = search_in(dir.path(), &query, 10, None).await.unwrap();
        assert_eq!(all.total_found, 3);
        let alpha = search_in(dir.path(), &query, 10, Some("alpha"))
            .await
            .unwrap();
        assert_eq!(alpha.total_found, 1);
        assert_eq!(
            alpha.matches[0].conversation_id,
            "01JVAAAAAAAAAAAAAAAAAAAAAA"
        );
    }
}
