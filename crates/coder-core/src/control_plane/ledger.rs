//! Durable append-only task ledger with crash recovery and one-writer locking.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use fs4::fs_std::FileExt;

use super::ids::validate_task_id;
use super::{ControlPlaneError, TaskEvent, TaskEventKind, TaskRecord};

/// An authoritative append-only ledger for a single task.
#[derive(Debug)]
pub struct TaskLedger {
    task_id: String,
    events: Vec<TaskEvent>,
    ledger_path: Option<PathBuf>,
}

struct LedgerLock(File);

impl Drop for LedgerLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

impl TaskLedger {
    /// Create a new in-memory ledger initialized with a `TaskCreated` event.
    pub fn new(initial_event: TaskEvent) -> Result<Self, ControlPlaneError> {
        if !matches!(initial_event.payload, TaskEventKind::TaskCreated { .. }) {
            return Err(ControlPlaneError::InvalidInitialEvent(format!(
                "{:?}",
                initial_event.payload
            )));
        }
        let task_id = initial_event.task_id.clone();
        Ok(Self {
            task_id,
            events: vec![initial_event],
            ledger_path: None,
        })
    }

    /// Create a disk-backed ledger under `<tasks_root>/<task_id>/ledger.jsonl`.
    ///
    /// If the ledger already exists for the same task ID, it is loaded from disk
    /// rather than overwritten.
    pub fn create_in(
        tasks_root: impl AsRef<Path>,
        initial_event: TaskEvent,
    ) -> Result<Self, ControlPlaneError> {
        validate_task_id(&initial_event.task_id)?;
        let created = Self::new(initial_event)?;
        let task_dir = tasks_root.as_ref().join(&created.task_id);
        std::fs::create_dir_all(&task_dir)?;
        let _lock = acquire_lock(&task_dir)?;
        let ledger_path = task_dir.join("ledger.jsonl");
        if ledger_path.exists() {
            let mut existing = load_unlocked(&ledger_path)?;
            if existing.task_id != created.task_id {
                return Err(ControlPlaneError::TaskIdMismatch {
                    event_task_id: created.task_id,
                    ledger_task_id: existing.task_id,
                });
            }
            existing.ledger_path = Some(ledger_path);
            return Ok(existing);
        }

        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&ledger_path)?;
        write_event(&mut file, &created.events[0])?;
        let mut ledger = created;
        ledger.ledger_path = Some(ledger_path);
        ledger.write_projection_cache()?;
        Ok(ledger)
    }

    /// Append a new event. Duplicate event ids or command ids are a no-op.
    pub fn append(&mut self, event: TaskEvent) -> Result<(), ControlPlaneError> {
        self.record(event)?;
        Ok(())
    }

    /// Record an event. Returns `true` when a new line was written.
    pub fn record(&mut self, event: TaskEvent) -> Result<bool, ControlPlaneError> {
        if event.task_id != self.task_id {
            return Err(ControlPlaneError::TaskIdMismatch {
                event_task_id: event.task_id,
                ledger_task_id: self.task_id.clone(),
            });
        }
        if self.is_duplicate(&event) {
            return Ok(false);
        }
        reject_lease_conflict(self, &event)?;
        if let Some(path) = self.ledger_path.clone() {
            let task_dir = path.parent().ok_or_else(|| {
                ControlPlaneError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "ledger path has no parent directory",
                ))
            })?;
            let _lock = acquire_lock(task_dir)?;
            let mut file = OpenOptions::new().append(true).open(&path)?;
            write_event(&mut file, &event)?;
            self.events.push(event);
            self.write_projection_cache()?;
            return Ok(true);
        }
        self.events.push(event);
        Ok(true)
    }

    /// Return the immutable slice of all recorded events.
    pub fn events(&self) -> &[TaskEvent] {
        &self.events
    }

    /// Project the events into a unified `TaskRecord`.
    pub fn project(&self) -> Result<TaskRecord, ControlPlaneError> {
        let first = self.events.first().ok_or(ControlPlaneError::EmptyHistory)?;
        let mut record = TaskRecord::from_task_created(first)?;

        for event in &self.events[1..] {
            record.apply(event);
        }

        Ok(record)
    }

    /// Serialize the append-only ledger to a JSONL writer.
    pub fn write_to_writer(&self, mut writer: impl Write) -> Result<(), std::io::Error> {
        for event in &self.events {
            let json = serde_json::to_string(event)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
            writeln!(writer, "{json}")?;
        }
        Ok(())
    }

    /// Deserialize a ledger from a JSONL reader.
    pub fn load_from_reader(reader: impl std::io::Read) -> Result<Self, ControlPlaneError> {
        load_from_lines(reader, None)
    }

    /// Load a disk-backed ledger and continue append-flushed persistence.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, ControlPlaneError> {
        let path = path.as_ref().to_path_buf();
        let task_dir = path.parent().ok_or_else(|| {
            ControlPlaneError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ledger path has no parent directory",
            ))
        })?;
        let _lock = acquire_lock(task_dir)?;
        let mut ledger = load_unlocked(&path)?;
        ledger.ledger_path = Some(path);
        Ok(ledger)
    }

    fn is_duplicate(&self, event: &TaskEvent) -> bool {
        if self
            .events
            .iter()
            .any(|existing| existing.event_id == event.event_id)
        {
            return true;
        }
        match event.command_id.as_deref() {
            Some(command_id) if !command_id.is_empty() => self
                .events
                .iter()
                .any(|existing| existing.command_id.as_deref() == Some(command_id)),
            _ => false,
        }
    }

    fn write_projection_cache(&self) -> Result<(), ControlPlaneError> {
        let Some(ledger_path) = &self.ledger_path else {
            return Ok(());
        };
        let task_path = ledger_path.with_file_name("task.json");
        let bytes = serde_json::to_vec_pretty(&self.project()?)?;
        atomic_write(&task_path, &bytes)?;
        Ok(())
    }
}

fn reject_lease_conflict(ledger: &TaskLedger, event: &TaskEvent) -> Result<(), ControlPlaneError> {
    let TaskEventKind::ControllerLeaseClaimed { controller } = &event.payload else {
        return Ok(());
    };
    if let Some(held) = held_controller(ledger)
        && held != controller
    {
        return Err(ControlPlaneError::ControllerLeaseConflict {
            held: held.to_string(),
            requested: controller.clone(),
        });
    }
    Ok(())
}

fn held_controller(ledger: &TaskLedger) -> Option<&str> {
    ledger
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            TaskEventKind::ControllerLeaseClaimed { controller } => Some(controller.as_str()),
            _ => None,
        })
}

fn acquire_lock(task_dir: &Path) -> Result<LedgerLock, ControlPlaneError> {
    let path = task_dir.join("ledger.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    FileExt::lock_exclusive(&file)?;
    Ok(LedgerLock(file))
}

fn load_unlocked(path: &Path) -> Result<TaskLedger, ControlPlaneError> {
    let file = File::open(path)?;
    let (ledger, truncated) = load_recovering(file)?;
    if truncated {
        persist_complete_events(path, &ledger.events)?;
    }
    Ok(ledger)
}

fn load_from_lines(
    reader: impl std::io::Read,
    ledger_path: Option<PathBuf>,
) -> Result<TaskLedger, ControlPlaneError> {
    let (mut ledger, _) = load_recovering(reader)?;
    ledger.ledger_path = ledger_path;
    Ok(ledger)
}

fn load_recovering(reader: impl std::io::Read) -> Result<(TaskLedger, bool), ControlPlaneError> {
    let buf = BufReader::new(reader);
    let mut events = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut truncated = false;
    let lines: Vec<String> = buf.lines().collect::<Result<_, _>>()?;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<TaskEvent>(trimmed) {
            Ok(event) => {
                if !seen.insert(event.event_id.clone()) {
                    return Err(ControlPlaneError::DuplicateEventId(event.event_id));
                }
                events.push(event);
            }
            Err(error) => {
                let rest_empty = lines[index + 1..].iter().all(|row| row.trim().is_empty());
                if rest_empty {
                    truncated = true;
                    break;
                }
                return Err(ControlPlaneError::Serialization(error));
            }
        }
    }

    let first = events.first().ok_or(ControlPlaneError::EmptyHistory)?;
    let task_id = first.task_id.clone();
    for event in &events {
        if event.task_id != task_id {
            return Err(ControlPlaneError::TaskIdMismatch {
                event_task_id: event.task_id.clone(),
                ledger_task_id: task_id,
            });
        }
    }
    if !matches!(first.payload, TaskEventKind::TaskCreated { .. }) {
        return Err(ControlPlaneError::InvalidInitialEvent(format!(
            "{:?}",
            first.payload
        )));
    }

    Ok((
        TaskLedger {
            task_id,
            events,
            ledger_path: None,
        },
        truncated,
    ))
}

fn persist_complete_events(path: &Path, events: &[TaskEvent]) -> Result<(), ControlPlaneError> {
    let tmp = path.with_extension("jsonl.recovered");
    {
        let mut file = File::create(&tmp)?;
        for event in events {
            write_event(&mut file, event)?;
        }
    }
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ControlPlaneError> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn write_event(file: &mut File, event: &TaskEvent) -> Result<(), ControlPlaneError> {
    serde_json::to_writer(&mut *file, event)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
