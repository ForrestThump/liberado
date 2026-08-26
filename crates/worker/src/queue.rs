//! The durable task queue: one directory per task under `<data>/delegate/tasks/`.
//!
//! Layout: `<task_id>/task.json` (the spec as received) and `<task_id>/record.json`
//! (status + session id + PR url). Both written atomically-ish (write then rename is not
//! needed at this scale: a torn record loses only status, never the work — branches and
//! commits live in git, traces in the worktree).
//!
//! Every transition also appends a [`WorkerEvent`] to `<task_id>/events.jsonl` with a
//! persisted monotonic sequence baked into its correlation id (`delegate:<id>:<seq>`),
//! then offers it to live subscribers. Replay-from-journal plus correlation ids is what
//! makes the event stream at-least-once-safe: a client that reconnects gets the full
//! history and deduplicates anything it saw twice.
//!
//! Idempotency lives here and nowhere else: `submit` keys on `TaskSpec.id`, so an
//! at-least-once redelivery returns the stored record with `duplicate = true` instead of
//! running the task twice. This is the same discipline the vault inbox uses.

use std::path::PathBuf;

use liberado_delegate_contract::{
    EventKind, SubmitOutcome, TaskId, TaskRecord, TaskSpec, TaskStatus, WorkerEvent,
};

/// A question and, when it has one, its answer. The storage unit under
/// `<task>/questions/<question_id>.json`; also what a resumed reader finds on disk
/// after either side of the exchange crashes.
#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    #[error("queue io: {0}")]
    Io(#[from] std::io::Error),
    #[error("queue json for {context}: {source}")]
    Json {
        context: String,
        source: serde_json::Error,
    },
    #[error("question {0} not found")]
    QuestionNotFound(String),
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

/// Broadcast capacity for live subscribers. Replay-from-disk covers everyone else;
/// a slow SSE client that falls this far behind mid-stream skips forward rather than
/// stalling the worker (`Lagged` handling in the stream), and its correlation ids let
/// it notice the gap.
const EVENT_CHANNEL_CAPACITY: usize = 256;

pub struct TaskStore {
    root: PathBuf,
    events: tokio::sync::broadcast::Sender<WorkerEvent>,
}

impl TaskStore {
    pub fn open(data_dir: &std::path::Path) -> Result<Self, QueueError> {
        let root = data_dir.join("delegate").join("tasks");
        std::fs::create_dir_all(&root)?;
        let (events, _) = tokio::sync::broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Ok(Self { root, events })
    }

    /// Live subscription to task events. Late joiners get history through
    /// [`TaskStore::replay`]; the stream handlers splice the two together.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<WorkerEvent> {
        self.events.subscribe()
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
        let record = self.persist_new_task(spec)?;
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
        self.transition(id, EventKind::StatusChanged, |record| {
            record.status = TaskStatus::Running;
            record.session_id = Some(session_id.to_string());
        })
    }

    /// Record a terminal status (`PrOpened` / `Failed` / `Cancelled`). The PR URL is
    /// lifted out of the variant so surfaces can render it without matching on status.
    /// A `PrOpened` transition emits [`EventKind::PrReady`] — the event the delegator's
    /// inbox wakes on; every other terminal lands as a plain status change.
    pub fn finish(&self, id: &TaskId, status: TaskStatus) -> Result<TaskRecord, QueueError> {
        let (kind, pr_url) = event_shape(&status);
        let record = self.transition(id, kind, |record| {
            record.status = status.clone();
            record.pr_url = pr_url;
        })?;
        Ok(record)
    }

    /// One state change plus its journalled event, applied together. Every public
    /// transition funnels through here so emission cannot be forgotten and this file's
    /// per-function complexity stays flat.
    fn transition(
        &self,
        id: &TaskId,
        kind: EventKind,
        change: impl FnOnce(&mut TaskRecord),
    ) -> Result<TaskRecord, QueueError> {
        let record = self.patch(id, change)?;
        self.record_event(id, kind, status_payload(&record.status))?;
        Ok(record)
    }

    /// First delivery: write the spec, the initial record, and the queued event.
    fn persist_new_task(&self, spec: &TaskSpec) -> Result<TaskRecord, QueueError> {
        let dir = self.task_dir(&spec.id);
        let record = TaskRecord {
            spec: spec.clone(),
            status: TaskStatus::Queued,
            session_id: None,
            pr_url: None,
            updated_at: now_rfc3339(),
        };
        self.write_json(&dir.join("task.json"), spec)?;
        self.write_json(&dir.join("record.json"), &record)?;
        self.record_event(
            &spec.id,
            EventKind::StatusChanged,
            status_payload(&record.status),
        )?;
        Ok(record)
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
            TaskStatus::PrOpened { .. }
            | TaskStatus::Failed { .. }
            | TaskStatus::Cancelled
            | TaskStatus::Blocked { .. } => Ok(record),
            // Running tasks refuse until cooperative stop lands; a kickback re-run
            // moves PrOpened back to Running through the answers endpoint instead.
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

    /// Every event recorded for a task, in emission order. Torn trailing lines (a
    /// crash mid-append) are skipped, not fatal: an at-least-once stream may lose a
    /// tail entry to infrastructure, and the status poll is the reconciliation path.
    pub fn replay(&self, id: &str) -> Result<Vec<WorkerEvent>, QueueError> {
        let path = self.root.join(id).join("events.jsonl");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let raw = std::fs::read_to_string(&path)?;
        Ok(raw
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect())
    }

    /// Record one event: persist to the task's journal with a monotonic correlation
    /// sequence, then offer it to live subscribers. Journal write first — replay is
    /// the durable contract, the broadcast is best-effort fan-out.
    fn record_event(
        &self,
        id: &TaskId,
        kind: EventKind,
        payload: serde_json::Value,
    ) -> Result<(), QueueError> {
        self.journal_event(self.mint_event(id, kind, payload)?)
    }

    /// Build the event without touching disk: monotonic sequence + correlation id.
    /// Callers that must embed the correlation into the payload (questions) mint
    /// first and journal second; everyone else uses [`TaskStore::record_event`].
    fn mint_event(
        &self,
        id: &TaskId,
        kind: EventKind,
        payload: serde_json::Value,
    ) -> Result<WorkerEvent, QueueError> {
        let seq = self.next_seq(id)?;
        Ok(WorkerEvent {
            kind,
            correlation_id: format!("delegate:{id}:{seq}"),
            task_id: id.clone(),
            payload,
        })
    }

    fn journal_event(&self, event: WorkerEvent) -> Result<(), QueueError> {
        let dir = self.task_dir(&event.task_id);
        use std::io::Write as _;
        let mut journal = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("events.jsonl"))?;
        writeln!(
            journal,
            "{}",
            serde_json::to_string(&event).map_err(|source| QueueError::Json {
                context: "event serialization".into(),
                source,
            })?
        )?;
        let _ = self.events.send(event);
        Ok(())
    }

    /// Monotonic per-task sequence, persisted so correlations stay stable across
    /// restarts. A process-global lock is fine here: transitions are rare (a few per
    /// task) and contended only in the same instant a task changes state.
    fn next_seq(&self, id: &TaskId) -> Result<u64, QueueError> {
        let _guard = SEQ_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = self.task_dir(id);
        let path = dir.join("events.seq");
        let current: u64 = match std::fs::read_to_string(&path) {
            Ok(raw) => raw.trim().parse().unwrap_or(0),
            Err(_) => 0,
        };
        let next = current + 1;
        std::fs::write(&path, next.to_string())?;
        Ok(next)
    }
}

/// Which event a terminal status announces, and what the record's PR field becomes.
/// A `PrOpened` transition is [`EventKind::PrReady`] — the delegator's wake signal;
/// every other transition (failed, cancelled, blocked alike) is a plain
/// `StatusChanged`. [`EventKind::Blocked`] stays reserved for mid-run blockage
/// markers, which do not close the stream; a terminal blocked *status* does, through
/// its state field.
fn event_shape(status: &TaskStatus) -> (EventKind, Option<String>) {
    match status {
        TaskStatus::PrOpened { url } => (EventKind::PrReady, Some(url.clone())),
        _ => (EventKind::StatusChanged, None),
    }
}

/// The wire shape of a status transition: the full [`TaskStatus`] under one key, so
/// consumers match on the same enum the poll endpoint returns.
fn status_payload(status: &TaskStatus) -> serde_json::Value {
    serde_json::json!({ "status": status })
}

static SEQ_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests;

#[path = "queue/questions.rs"]
mod questions;
