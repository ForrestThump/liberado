//! The durable task queue: one directory per task under `<data>/delegate/tasks/`.
//!
//! Layout: `<task_id>/task.json` (the spec as received) and `<task_id>/record.json`
//! (status + session id + PR url). Both written atomically-ish (write then rename is not
//! needed at this scale: a torn record loses only status, never the work — branches and
//! commits live in git, traces in the worktree).
//!
//! Idempotency lives here and nowhere else: `submit` keys on `TaskSpec.id`, so an
//! at-least-once redelivery returns the stored record with `duplicate = true` instead of
//! running the task twice. This is the same discipline the vault inbox uses.

use std::path::PathBuf;

use liberado_delegate_contract::{SubmitOutcome, TaskId, TaskRecord, TaskSpec, TaskStatus};

#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    #[error("queue io: {0}")]
    Io(#[from] std::io::Error),
    #[error("queue json for {context}: {source}")]
    Json {
        context: String,
        source: serde_json::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum CancelError {
    #[error("task {0} not found")]
    NotFound(String),
    /// Cooperative stop mid-run arrives with park/resume (D2); refusing beats silently
    /// ignoring the request while the run keeps going.
    #[error("task {0} is running; cooperative cancel is not available yet (lands in D2)")]
    Running(String),
    #[error(transparent)]
    Queue(#[from] QueueError),
}

pub struct TaskStore {
    root: PathBuf,
}

impl TaskStore {
    pub fn open(data_dir: &std::path::Path) -> Result<Self, QueueError> {
        let root = data_dir.join("delegate").join("tasks");
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn task_dir(&self, id: &TaskId) -> PathBuf {
        self.root.join(&id.0)
    }

    fn write_json(
        &self,
        path: &std::path::Path,
        value: &impl serde::Serialize,
    ) -> Result<(), QueueError> {
        let bytes = serde_json::to_vec_pretty(value).map_err(|source| QueueError::Json {
            context: path.display().to_string(),
            source,
        })?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    fn read_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &std::path::Path,
    ) -> Result<T, QueueError> {
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(|source| QueueError::Json {
            context: path.display().to_string(),
            source,
        })
    }

    /// Persist a submitted spec. First delivery stores it and returns
    /// `duplicate = false`; any later delivery of the same id returns the stored record
    /// untouched with `duplicate = true`.
    pub fn submit(&self, spec: &TaskSpec) -> Result<SubmitOutcome, QueueError> {
        let dir = self.task_dir(&spec.id);
        if dir.exists() {
            let existing: TaskRecord = self.read_json(&dir.join("record.json"))?;
            return Ok(SubmitOutcome {
                record: existing,
                duplicate: true,
            });
        }
        std::fs::create_dir_all(&dir)?;
        self.write_json(&dir.join("task.json"), spec)?;
        let record = TaskRecord {
            spec: spec.clone(),
            status: TaskStatus::Queued,
            session_id: None,
            pr_url: None,
            updated_at: now_rfc3339(),
        };
        self.write_json(&dir.join("record.json"), &record)?;
        Ok(SubmitOutcome {
            record,
            duplicate: false,
        })
    }

    pub fn get(&self, id: &str) -> Result<Option<TaskRecord>, QueueError> {
        let path = self.root.join(id).join("record.json");
        if !path.exists() {
            return Ok(None);
        }
        self.read_json(&path).map(Some)
    }

    /// Move a task into `Running`, recording the coding session id.
    pub fn mark_running(&self, id: &TaskId, session_id: &str) -> Result<TaskRecord, QueueError> {
        self.patch(id, |record| {
            record.status = TaskStatus::Running;
            record.session_id = Some(session_id.to_string());
        })
    }

    /// Record a terminal status (`PrOpened` / `Failed` / `Cancelled`). The PR URL is
    /// lifted out of the variant so surfaces can render it without matching on status.
    pub fn finish(&self, id: &TaskId, status: TaskStatus) -> Result<TaskRecord, QueueError> {
        let pr_url = match &status {
            TaskStatus::PrOpened { url } => Some(url.clone()),
            _ => None,
        };
        self.patch(id, |record| {
            record.status = status.clone();
            record.pr_url = pr_url.clone();
        })
    }

    fn patch(
        &self,
        id: &TaskId,
        change: impl FnOnce(&mut TaskRecord),
    ) -> Result<TaskRecord, QueueError> {
        let dir = self.task_dir(id);
        let mut record: TaskRecord = self.read_json(&dir.join("record.json"))?;
        change(&mut record);
        record.updated_at = now_rfc3339();
        self.write_json(&dir.join("record.json"), &record)?;
        Ok(record)
    }

    /// Queue-level cancel. Terminal states are idempotent no-ops; running tasks refuse.
    pub fn cancel(&self, id: &str) -> Result<TaskRecord, CancelError> {
        let mut record = self
            .get(id)?
            .ok_or_else(|| CancelError::NotFound(id.to_string()))?;
        match record.status {
            TaskStatus::Queued => {
                let id = TaskId(id.to_string());
                record = self.finish(&id, TaskStatus::Cancelled)?;
                Ok(record)
            }
            TaskStatus::PrOpened { .. } | TaskStatus::Failed { .. } | TaskStatus::Cancelled => {
                Ok(record)
            }
            TaskStatus::Running => Err(CancelError::Running(id.to_string())),
        }
    }

    /// Ids whose task.json exists — the restart-rescan universe.
    pub fn known_ids(&self) -> Result<Vec<String>, QueueError> {
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.path().join("task.json").exists() {
                ids.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        ids.sort();
        Ok(ids)
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests;
