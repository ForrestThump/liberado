//! Comment-preserving writer for `[[schedules]]` in `topology.toml`.
//!
//! The daemon copies schedules into the cron task at boot and does not replace that task.
//! This module writes the file only. Callers must recreate the process before cron or
//! `GET /api/schedules` show the new rows. See `docs/spec/config-spec.md` §4.5.

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

use super::{ConfigError, CronSchedule, load_config};

const SECONDS_PER_DAY: u64 = 86_400;

/// What a successful [`create_schedule`], [`update_schedule`], or [`delete_schedule`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleWriteAction {
    /// A new `[[schedules]]` row was appended.
    Created,
    /// The row with the given name was replaced.
    Updated,
    /// The row with the given name was removed.
    Deleted,
}

/// The file change, plus the backup that was taken before the edit.
///
/// [`Self::process_recreate_required`] is always true. This writer does not reload cron
/// or the in-memory config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleWriteReport {
    /// Whether the named row was created, replaced, or removed.
    pub action: ScheduleWriteAction,
    /// The schedule name after the edit. For a delete, the name that was removed.
    pub name: String,
    /// The `topology.toml` that was replaced.
    pub path: PathBuf,
    /// The timestamped sibling copy of the pre-edit file.
    pub backup_path: PathBuf,
    /// Always `true`. Cron and the schedules list keep the boot snapshot.
    pub process_recreate_required: bool,
}

impl ScheduleWriteReport {
    /// One sentence a caller can show: what changed, where the backup is, and that a
    /// process recreate is required before cron sees the file.
    pub fn summary(&self) -> String {
        let verb = match self.action {
            ScheduleWriteAction::Created => "Created",
            ScheduleWriteAction::Updated => "Updated",
            ScheduleWriteAction::Deleted => "Deleted",
        };
        format!(
            "{verb} schedule '{}' in {}. Backup: {}. The file is on disk. Cron and the schedules list still show the boot copy until the process is recreated.",
            self.name,
            self.path.display(),
            self.backup_path.display()
        )
    }
}

impl std::fmt::Display for ScheduleWriteReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.summary())
    }
}

/// Why a schedule write stopped. The original `topology.toml` is unchanged in every case.
#[derive(Debug, Error)]
pub enum ScheduleWriteError {
    /// The file is missing, or this process cannot open it for write. Nothing was copied.
    #[error("topology file '{path}' is not writable ({reason}); wrote nothing")]
    NotWritable {
        /// The path that was refused.
        path: PathBuf,
        /// The operating-system or existence reason.
        reason: String,
    },

    /// `config_dir()` found no directory. Nothing was written.
    #[error("no config directory is resolved (set LIBERADO_CONFIG_DIR); wrote nothing")]
    NoConfigDir,

    /// The timestamped copy failed. The original file was not changed.
    #[error("failed to back up '{path}' to '{backup}': {source}; original file was not changed")]
    Backup {
        /// The file that was not changed.
        path: PathBuf,
        /// The sibling that could not be written.
        backup: PathBuf,
        /// The copy error.
        #[source]
        source: io::Error,
    },

    /// The file could not be read after the backup. The original was not changed.
    #[error(
        "failed to read '{path}': {source}; original file was not changed; backup kept at '{backup}'"
    )]
    Read {
        /// The file that was not changed.
        path: PathBuf,
        /// The backup that was kept.
        backup: PathBuf,
        /// The read error.
        #[source]
        source: io::Error,
    },

    /// The `[[schedules]]` splice was refused. The original was not changed.
    #[error(
        "cannot edit [[schedules]] in '{path}': {detail}; original file was not changed; backup kept at '{backup}'"
    )]
    Edit {
        /// The file that was not changed.
        path: PathBuf,
        /// The backup that was kept.
        backup: PathBuf,
        /// Why this edit does not apply.
        detail: String,
    },

    /// The scratch copy used for [`load_config`] could not be prepared.
    #[error(
        "could not check the edited topology (original file was not changed; backup kept at '{backup}'): {source}"
    )]
    Check {
        /// The backup that was kept.
        backup: PathBuf,
        /// The scratch-directory error.
        #[source]
        source: io::Error,
    },

    /// Boot validation rejected the edited file. The original was not changed.
    #[error(
        "edited topology.toml failed validation; original file was not changed; backup kept at '{backup}': {source}"
    )]
    Invalid {
        /// The backup that was kept.
        backup: PathBuf,
        /// The loader or [`crate::Config::validate`] error.
        #[source]
        source: Box<ConfigError>,
    },

    /// The temp file or the rename failed. The original was not changed.
    #[error(
        "failed to replace '{path}': {source}; original file was not changed; backup kept at '{backup}'"
    )]
    Write {
        /// The file that was not changed.
        path: PathBuf,
        /// The backup that was kept.
        backup: PathBuf,
        /// The temp-write or rename error.
        #[source]
        source: io::Error,
    },
}

/// `topology.toml` inside the resolved [`super::config_dir`].
///
/// Does not check that the file exists or is writable. [`create_schedule`] does that.
pub fn schedule_topology_path() -> Result<PathBuf, ScheduleWriteError> {
    let dir = super::config_dir().ok_or(ScheduleWriteError::NoConfigDir)?;
    Ok(dir.join(super::TOPOLOGY_FILE))
}

/// Append one `[[schedules]]` row.
///
/// `path` is `topology.toml`, or the config directory that contains it.
/// A row with the same `name` is refused. The original file stays in place until
/// validation passes and the rename commits.
pub fn create_schedule(
    path: &Path,
    schedule: CronSchedule,
) -> Result<ScheduleWriteReport, ScheduleWriteError> {
    apply_schedule_edit(path, ScheduleEdit::Create(schedule))
}

/// Replace the `[[schedules]]` row named `name`.
///
/// `schedule.name` is the name written into the file. It may differ from `name`
/// (a rename). A missing row is refused. Validation still rejects a name that
/// collides with a different row.
pub fn update_schedule(
    path: &Path,
    name: &str,
    schedule: CronSchedule,
) -> Result<ScheduleWriteReport, ScheduleWriteError> {
    apply_schedule_edit(
        path,
        ScheduleEdit::Update {
            name: name.to_string(),
            schedule,
        },
    )
}

/// Remove the `[[schedules]]` row named `name`. A missing row is refused.
pub fn delete_schedule(path: &Path, name: &str) -> Result<ScheduleWriteReport, ScheduleWriteError> {
    apply_schedule_edit(
        path,
        ScheduleEdit::Delete {
            name: name.to_string(),
        },
    )
}

enum ScheduleEdit {
    Create(CronSchedule),
    Update {
        name: String,
        schedule: CronSchedule,
    },
    Delete {
        name: String,
    },
}

fn apply_schedule_edit(
    path: &Path,
    edit: ScheduleEdit,
) -> Result<ScheduleWriteReport, ScheduleWriteError> {
    let path = resolve_topology_file(path);
    ensure_writable(&path)?;
    let backup = backup_topology(&path)?;
    let original = std::fs::read_to_string(&path).map_err(|source| ScheduleWriteError::Read {
        path: path.clone(),
        backup: backup.clone(),
        source,
    })?;
    let edited = splice_text(&original, &edit).map_err(|detail| ScheduleWriteError::Edit {
        path: path.clone(),
        backup: backup.clone(),
        detail,
    })?;
    validate_edited(&path, &edited, &backup)?;
    write_atomic(&path, edited.as_bytes()).map_err(|source| ScheduleWriteError::Write {
        path: path.clone(),
        backup: backup.clone(),
        source,
    })?;
    Ok(report(&path, backup, edit))
}

fn resolve_topology_file(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join(super::TOPOLOGY_FILE)
    } else {
        path.to_path_buf()
    }
}

fn ensure_writable(path: &Path) -> Result<(), ScheduleWriteError> {
    if !path.is_file() {
        return Err(ScheduleWriteError::NotWritable {
            path: path.to_path_buf(),
            reason: "file does not exist".into(),
        });
    }
    // Opening for write (no truncate) fails on a read-only file and on a read-only
    // mount. Rename would still succeed on Unix when only the mode bit is clear and
    // the directory is writable, so this check has to happen before the backup.
    match OpenOptions::new().write(true).open(path) {
        Ok(_) => Ok(()),
        Err(err) => Err(ScheduleWriteError::NotWritable {
            path: path.to_path_buf(),
            reason: err.to_string(),
        }),
    }
}

fn backup_topology(path: &Path) -> Result<PathBuf, ScheduleWriteError> {
    let backup = backup_destination(path);
    if let Err(source) = std::fs::copy(path, &backup) {
        let _ = std::fs::remove_file(&backup);
        return Err(ScheduleWriteError::Backup {
            path: path.to_path_buf(),
            backup,
            source,
        });
    }
    Ok(backup)
}

fn backup_destination(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| super::TOPOLOGY_FILE.to_string());
    let parent = parent_dir(path);
    let stamp = utc_stamp();
    let mut candidate = parent.join(format!("{file_name}.bak-{stamp}"));
    let mut n = 2u32;
    while candidate.exists() && n < 1000 {
        candidate = parent.join(format!("{file_name}.bak-{stamp}-{n}"));
        n += 1;
    }
    candidate
}

fn splice_text(original: &str, edit: &ScheduleEdit) -> Result<String, String> {
    let mut doc = original
        .parse::<DocumentMut>()
        .map_err(|err| format!("topology.toml is not valid TOML: {err}"))?;
    match edit {
        ScheduleEdit::Create(schedule) => create_row(&mut doc, schedule)?,
        ScheduleEdit::Update { name, schedule } => update_row(&mut doc, name, schedule)?,
        ScheduleEdit::Delete { name } => delete_row(&mut doc, name)?,
    }
    Ok(doc.to_string())
}

fn create_row(doc: &mut DocumentMut, schedule: &CronSchedule) -> Result<(), String> {
    let table = schedule_table(schedule)?;
    let array = schedules_mut(doc, true, &schedule.name)?;
    if find_index(array, &schedule.name).is_some() {
        return Err(format!("schedule '{}' already exists", schedule.name));
    }
    array.push(table);
    Ok(())
}

fn update_row(doc: &mut DocumentMut, name: &str, schedule: &CronSchedule) -> Result<(), String> {
    let table = schedule_table(schedule)?;
    let array = schedules_mut(doc, false, name)?;
    let Some(index) = find_index(array, name) else {
        return Err(format!("schedule '{name}' was not found"));
    };
    let Some(slot) = array.get_mut(index) else {
        return Err(format!("schedule '{name}' was not found"));
    };
    let decor = slot.decor().clone();
    *slot = table;
    *slot.decor_mut() = decor;
    Ok(())
}

fn delete_row(doc: &mut DocumentMut, name: &str) -> Result<(), String> {
    let array = schedules_mut(doc, false, name)?;
    let Some(index) = find_index(array, name) else {
        return Err(format!("schedule '{name}' was not found"));
    };
    array.remove(index);
    Ok(())
}

fn schedules_mut<'a>(
    doc: &'a mut DocumentMut,
    create: bool,
    name: &str,
) -> Result<&'a mut ArrayOfTables, String> {
    let root = doc.as_table_mut();
    if root.get("schedules").is_none() {
        if !create {
            return Err(format!("schedule '{name}' was not found"));
        }
        root.insert("schedules", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    match root.get_mut("schedules") {
        Some(item) => item.as_array_of_tables_mut().ok_or_else(|| {
            "topology.toml key 'schedules' must be [[schedules]] tables".to_string()
        }),
        None => Err("topology.toml key 'schedules' must be [[schedules]] tables".into()),
    }
}

fn find_index(array: &ArrayOfTables, name: &str) -> Option<usize> {
    array
        .iter()
        .position(|table| table.get("name").and_then(Item::as_str) == Some(name))
}

fn schedule_table(schedule: &CronSchedule) -> Result<Table, String> {
    let text = toml::to_string(schedule)
        .map_err(|err| format!("could not serialize schedule '{}': {err}", schedule.name))?;
    let doc = text.parse::<DocumentMut>().map_err(|err| {
        format!(
            "could not parse serialized schedule '{}': {err}",
            schedule.name
        )
    })?;
    let mut table = doc.as_table().clone();
    table.set_implicit(false);
    table.decor_mut().set_prefix("\n");
    Ok(table)
}

fn validate_edited(topology: &Path, edited: &str, backup: &Path) -> Result<(), ScheduleWriteError> {
    let scratch = ScratchDir::new().map_err(|source| ScheduleWriteError::Check {
        backup: backup.to_path_buf(),
        source,
    })?;
    let parent = parent_dir(topology);
    copy_if_present(
        &parent.join(super::POLICY_FILE),
        &scratch.path.join(super::POLICY_FILE),
        backup,
    )?;
    copy_if_present(
        &parent.join(super::TUNING_FILE),
        &scratch.path.join(super::TUNING_FILE),
        backup,
    )?;
    std::fs::write(scratch.path.join(super::TOPOLOGY_FILE), edited).map_err(|source| {
        ScheduleWriteError::Check {
            backup: backup.to_path_buf(),
            source,
        }
    })?;
    // Same loader as boot: `load_config` runs `Config::validate` (schedules, pools,
    // profiles, and the rest) and the merged-config checks. The live file is not
    // the one being parsed.
    load_config(Some(&scratch.path))
        .map(|_| ())
        .map_err(|source| ScheduleWriteError::Invalid {
            backup: backup.to_path_buf(),
            source: Box::new(source),
        })
}

fn copy_if_present(from: &Path, to: &Path, backup: &Path) -> Result<(), ScheduleWriteError> {
    if !from.is_file() {
        return Ok(());
    }
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|source| ScheduleWriteError::Check {
            backup: backup.to_path_buf(),
            source,
        })
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, bytes)?;
    if let Ok(perms) = std::fs::metadata(path).map(|meta| meta.permissions()) {
        let _ = std::fs::set_permissions(&tmp, perms);
    }
    if let Err(err) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

fn report(path: &Path, backup: PathBuf, edit: ScheduleEdit) -> ScheduleWriteReport {
    let (action, name) = match edit {
        ScheduleEdit::Create(schedule) => (ScheduleWriteAction::Created, schedule.name),
        ScheduleEdit::Update { schedule, .. } => (ScheduleWriteAction::Updated, schedule.name),
        ScheduleEdit::Delete { name } => (ScheduleWriteAction::Deleted, name),
    };
    ScheduleWriteReport {
        action,
        name,
        path: path.to_path_buf(),
        backup_path: backup,
        process_recreate_required: true,
    }
}

fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn utc_stamp() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    utc_stamp_from_unix(dur.as_secs(), dur.subsec_nanos())
}

fn utc_stamp_from_unix(secs: u64, nanos: u32) -> String {
    let days = (secs / SECONDS_PER_DAY) as i64;
    let tod = secs % SECONDS_PER_DAY;
    let (year, month, day) = civil_from_days(days);
    let hour = tod / 3600;
    let minute = (tod % 3600) / 60;
    let second = tod % 60;
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}.{nanos:09}Z")
}

/// Howard Hinnant's `civil_from_days`. `days` is days since 1970-01-01.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    (year as i32, month as u32, day as u32)
}

struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new() -> io::Result<Self> {
        let base = std::env::temp_dir();
        for n in 0..8 {
            let path = base.join(format!(
                "liberado-schedule-{}-{}-{n}",
                std::process::id(),
                utc_stamp()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create a schedule validation directory",
        ))
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
#[path = "schedule_write_tests.rs"]
mod tests;
