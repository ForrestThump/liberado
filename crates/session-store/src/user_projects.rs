//! User-created chat projects.
//!
//! Topology `[[projects]]` stay in config and keep their roots. A project created
//! in the WebUI is a row in `user-projects.json` beside the session logs. It has
//! no root and no write class, so it cannot authorize a coding workspace.
//!
//! The file's `schema_version` is [`USER_PROJECTS_SCHEMA`]. A missing version, or a
//! bare array from the first draft of this catalog, is rewritten as that version.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use liberado_conversation_store::{ChatMetaUpdate, ConversationStore};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::SessionStore;

/// On-disk catalog version. Bump it only with a migration in [`migrate_catalog_json`].
pub const USER_PROJECTS_SCHEMA: u32 = 1;

pub(crate) const FILE_NAME: &str = "user-projects.json";

const MAX_NAME_CHARS: usize = 80;
const MAX_INSTRUCTIONS_CHARS: usize = 2_000;

/// One project a person created. Chats store `id`, so a rename does not detach them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserProject {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A chat that belongs to a project, for the project page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectChat {
    pub id: String,
    pub title: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectWriteError {
    NotFound,
    Invalid(String),
    Unavailable(String),
}

impl std::fmt::Display for ProjectWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "project not found"),
            Self::Invalid(message) => write!(f, "{message}"),
            Self::Unavailable(message) => write!(f, "{message}"),
        }
    }
}

#[derive(Debug)]
pub(crate) struct UserCatalog {
    projects: Vec<UserProject>,
    /// A file we could not read. Saves must not replace it with an empty catalog.
    broken: bool,
}

#[derive(Serialize, Deserialize)]
struct DiskCatalog {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    projects: Vec<UserProject>,
}

impl UserCatalog {
    pub(crate) fn empty() -> Self {
        Self {
            projects: Vec::new(),
            broken: false,
        }
    }

    fn broken() -> Self {
        Self {
            projects: Vec::new(),
            broken: true,
        }
    }
}

/// Read a catalog document.
///
/// The bool is true when the bytes were an older shape and should be rewritten.
pub fn migrate_catalog_json(raw: &str) -> Result<(Vec<UserProject>, bool), String> {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    if value.is_array() {
        let projects = serde_json::from_value(value).map_err(|e| e.to_string())?;
        return Ok((projects, true));
    }
    let disk: DiskCatalog = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if disk.schema_version > USER_PROJECTS_SCHEMA {
        return Err(format!(
            "user projects schema {} is newer than {USER_PROJECTS_SCHEMA}",
            disk.schema_version
        ));
    }
    let rewritten = disk.schema_version != USER_PROJECTS_SCHEMA;
    Ok((disk.projects, rewritten))
}

pub(crate) fn load_at(dir: &Path) -> UserCatalog {
    let path = dir.join(FILE_NAME);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return UserCatalog::empty(),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "user projects catalog unreadable");
            return UserCatalog::broken();
        }
    };
    match migrate_catalog_json(&raw) {
        Ok((projects, rewritten)) => {
            let catalog = UserCatalog {
                projects,
                broken: false,
            };
            if rewritten {
                let _ = write_catalog(&path, &catalog.projects);
            }
            catalog
        }
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "user projects catalog unreadable");
            UserCatalog::broken()
        }
    }
}

fn write_catalog(path: &Path, projects: &[UserProject]) -> Result<(), ProjectWriteError> {
    let disk = DiskCatalog {
        schema_version: USER_PROJECTS_SCHEMA,
        projects: projects.to_vec(),
    };
    let body = serde_json::to_vec_pretty(&disk)
        .map_err(|error| ProjectWriteError::Unavailable(error.to_string()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| ProjectWriteError::Unavailable(error.to_string()))?;
    }
    let tmp = temp_path(path);
    std::fs::write(&tmp, body)
        .map_err(|error| ProjectWriteError::Unavailable(error.to_string()))?;
    std::fs::rename(&tmp, path).map_err(|error| ProjectWriteError::Unavailable(error.to_string()))
}

fn temp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

fn validate_name(name: &str) -> Result<String, ProjectWriteError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ProjectWriteError::Invalid("enter a project name".into()));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(ProjectWriteError::Invalid(format!(
            "keep the name under {MAX_NAME_CHARS} characters"
        )));
    }
    if name.contains('/') || name.contains('\\') {
        return Err(ProjectWriteError::Invalid(
            "a project name cannot contain a slash".into(),
        ));
    }
    Ok(name.to_string())
}

fn normalize_instructions(instructions: Option<&str>) -> Result<Option<String>, ProjectWriteError> {
    let Some(instructions) = instructions else {
        return Ok(None);
    };
    let instructions = instructions.trim();
    if instructions.is_empty() {
        return Ok(None);
    }
    if instructions.chars().count() > MAX_INSTRUCTIONS_CHARS {
        return Err(ProjectWriteError::Invalid(format!(
            "keep the instructions under {MAX_INSTRUCTIONS_CHARS} characters"
        )));
    }
    Ok(Some(instructions.to_string()))
}

fn name_taken(projects: &[UserProject], name: &str, except: Option<&str>) -> bool {
    projects.iter().any(|project| {
        except != Some(project.id.as_str()) && project.name.eq_ignore_ascii_case(name)
    })
}

impl SessionStore {
    fn catalog_path(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|dir| dir.join(FILE_NAME))
    }

    fn persist_locked(&self, projects: &[UserProject]) -> Result<(), ProjectWriteError> {
        let Some(path) = self.catalog_path() else {
            return Ok(());
        };
        write_catalog(&path, projects)
    }

    pub async fn list_user_projects(&self) -> Result<Vec<UserProject>, ProjectWriteError> {
        let catalog = self.projects.lock().await;
        if catalog.broken {
            return Err(ProjectWriteError::Unavailable(
                "project catalog is unreadable".into(),
            ));
        }
        Ok(catalog.projects.clone())
    }

    pub async fn user_project(&self, id: &str) -> Option<UserProject> {
        let catalog = self.projects.lock().await;
        catalog
            .projects
            .iter()
            .find(|project| project.id == id)
            .cloned()
    }

    pub async fn create_user_project(
        &self,
        name: &str,
        instructions: Option<&str>,
    ) -> Result<UserProject, ProjectWriteError> {
        let name = validate_name(name)?;
        let instructions = normalize_instructions(instructions)?;
        let mut catalog = self.projects.lock().await;
        if catalog.broken {
            return Err(ProjectWriteError::Unavailable(
                "project catalog is unreadable".into(),
            ));
        }
        if name_taken(&catalog.projects, &name, None) {
            return Err(ProjectWriteError::Invalid(format!(
                "a project named {name} already exists"
            )));
        }
        let now = Utc::now();
        let project = UserProject {
            id: Ulid::new().to_string(),
            name,
            instructions,
            created_at: now,
            updated_at: now,
        };
        catalog.projects.push(project.clone());
        if let Err(error) = self.persist_locked(&catalog.projects) {
            catalog.projects.pop();
            return Err(error);
        }
        Ok(project)
    }

    pub async fn update_user_project(
        &self,
        id: &str,
        name: Option<&str>,
        instructions: Option<Option<&str>>,
    ) -> Result<UserProject, ProjectWriteError> {
        let name = match name {
            Some(name) => Some(validate_name(name)?),
            None => None,
        };
        let instructions = match instructions {
            Some(value) => Some(normalize_instructions(value)?),
            None => None,
        };
        let mut catalog = self.projects.lock().await;
        if catalog.broken {
            return Err(ProjectWriteError::Unavailable(
                "project catalog is unreadable".into(),
            ));
        }
        let Some(index) = catalog.projects.iter().position(|project| project.id == id) else {
            return Err(ProjectWriteError::NotFound);
        };
        if let Some(name) = name.as_deref()
            && name_taken(&catalog.projects, name, Some(id))
        {
            return Err(ProjectWriteError::Invalid(format!(
                "a project named {name} already exists"
            )));
        }
        let snapshot = catalog.projects.clone();
        let project = &mut catalog.projects[index];
        if let Some(name) = name {
            project.name = name;
        }
        if let Some(instructions) = instructions {
            project.instructions = instructions;
        }
        project.updated_at = Utc::now();
        let updated = project.clone();
        if let Err(error) = self.persist_locked(&catalog.projects) {
            catalog.projects = snapshot;
            return Err(error);
        }
        Ok(updated)
    }

    /// Remove a user project and clear `project` on every chat that stored its id.
    ///
    /// The chat logs stay. A missing id is [`ProjectWriteError::NotFound`].
    pub async fn delete_user_project(&self, id: &str) -> Result<u32, ProjectWriteError> {
        {
            let catalog = self.projects.lock().await;
            if catalog.broken {
                return Err(ProjectWriteError::Unavailable(
                    "project catalog is unreadable".into(),
                ));
            }
            if !catalog.projects.iter().any(|project| project.id == id) {
                return Err(ProjectWriteError::NotFound);
            }
        }
        let detached = self.detach_project(id).await?;
        let mut catalog = self.projects.lock().await;
        let snapshot = catalog.projects.clone();
        catalog.projects.retain(|project| project.id != id);
        if let Err(error) = self.persist_locked(&catalog.projects) {
            catalog.projects = snapshot;
            return Err(error);
        }
        Ok(detached)
    }

    async fn detach_project(&self, id: &str) -> Result<u32, ProjectWriteError> {
        let ids: Vec<Ulid> = {
            let map = self.inner.lock().await;
            map.iter()
                .filter(|(_, live)| live.header.project.as_deref() == Some(id))
                .map(|(id, _)| *id)
                .collect()
        };
        let mut detached = 0u32;
        for conversation in ids {
            ConversationStore::set_chat_meta(
                self,
                conversation,
                ChatMetaUpdate {
                    project: Some(None),
                    pending_model: None,
                },
            )
            .await
            .map_err(|error| ProjectWriteError::Unavailable(error.to_string()))?;
            detached += 1;
        }
        Ok(detached)
    }

    /// Foreground chats stored on `project`, newest activity first.
    pub async fn project_chats(&self, project: &str) -> Vec<ProjectChat> {
        let map = self.inner.lock().await;
        let mut rows = Vec::new();
        for live in map.values() {
            if live.header.visibility.is_background() {
                continue;
            }
            if live.header.project.as_deref() != Some(project) {
                continue;
            }
            rows.push(ProjectChat {
                id: live.header.id.to_string(),
                title: live.header.title.clone(),
                updated_at: last_activity(&live.header, &live.nodes).to_rfc3339(),
            });
        }
        rows.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        rows
    }
}

fn last_activity(
    header: &crate::types::SessionHeader,
    nodes: &[liberado_conversation_store::MessageNode],
) -> DateTime<Utc> {
    nodes
        .iter()
        .map(|node| node.created_at)
        .max()
        .unwrap_or(header.created_at)
}

#[cfg(test)]
#[path = "user_projects_tests.rs"]
mod tests;
