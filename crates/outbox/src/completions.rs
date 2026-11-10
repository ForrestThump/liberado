//! Completion rows and the per-note snapshot in `liberado.db`.
//!
//! Rows are kept. `Outbox::prune` does not read these tables. A unique index
//! on `outbox_id` makes a Done tap idempotent. Tick rows leave `outbox_id`
//! null, and SQLite allows many nulls.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::completion_diff::TaskCounts;
use crate::completion_time::{
    CompletionTiming, DoneHint, PRECISION_TAP, SOURCE_LIBERADO, completion_timing,
};
use crate::error::Result;
use crate::source::OutboxSource;
use crate::store::Outbox;
use crate::time::{parse_stamp, stamp};

/// How far back a `resolution=completed` row may still supply tap time.
pub const DONE_HINT_MINUTES: i64 = 15;

/// One completion the tick or a Done tap wants to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCompletion {
    pub completed_at: DateTime<Utc>,
    pub local_date: String,
    pub precision: String,
    pub source: String,
    pub path: String,
    pub task_text: String,
    pub done_date: Option<String>,
    pub outbox_id: Option<i64>,
    pub detected_at: DateTime<Utc>,
}

/// One stored completion, including a voided row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskCompletion {
    pub id: i64,
    pub completed_at: String,
    pub local_date: String,
    pub precision: String,
    pub source: String,
    pub path: String,
    pub task_text: String,
    pub done_date: Option<String>,
    pub outbox_id: Option<i64>,
    pub voided_at: Option<String>,
    pub detected_at: String,
}

/// Completions to insert for one identity after the count diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionDraft {
    pub task_text: String,
    pub count: u32,
    pub prior_closed: i64,
    pub hint_text: Option<String>,
    pub done_date: Option<NaiveDate>,
}

/// Live rows to void for one identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoidDraft {
    pub task_text: String,
    pub count: u32,
}

/// One note's snapshot write: counts, new rows, and voids.
pub struct NoteApply<'a> {
    pub path: &'a str,
    pub fingerprint: &'a str,
    pub counts: &'a BTreeMap<String, TaskCounts>,
    pub completions: &'a [CompletionDraft],
    pub voids: &'a [VoidDraft],
    pub detected_at: DateTime<Utc>,
    pub zone: &'a str,
}

impl Outbox {
    /// Insert one row. A repeated `outbox_id` inserts nothing and returns false.
    pub fn insert_completion(&self, row: &NewCompletion) -> Result<bool> {
        let conn = self.lock();
        insert_completion_on(&conn, row)
    }

    /// Newest unused Done hint for this note and text inside the hint window.
    pub fn find_done_hint(
        &self,
        path: &str,
        text: &str,
        detected_at: DateTime<Utc>,
    ) -> Result<Option<DoneHint>> {
        let conn = self.lock();
        find_done_hint_on(&conn, path, text, detected_at)
    }

    /// Saved counts for one note. An unseen note is an empty map.
    pub fn snapshot_counts(&self, path: &str) -> Result<BTreeMap<String, TaskCounts>> {
        let conn = self.lock();
        snapshot_counts_on(&conn, path)
    }

    /// True when this note's bytes were already recorded.
    pub fn same_fingerprint(&self, path: &str, fingerprint: &str) -> Result<bool> {
        let conn = self.lock();
        let stored: Option<String> = conn
            .query_row(
                "SELECT fingerprint FROM task_file_state WHERE path = ?1",
                [path],
                |row| row.get(0),
            )
            .optional()?;
        Ok(stored.as_deref() == Some(fingerprint))
    }

    /// Apply one note's diff, snapshot, and fingerprint in one transaction.
    pub fn apply_completion_note(&self, note: &NoteApply<'_>) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        apply_note_on(&tx, note)?;
        tx.commit()?;
        Ok(())
    }

    /// Completions with `completed_at >= since`, oldest first.
    pub fn list_completions(&self, since: Option<&str>, limit: u32) -> Result<Vec<TaskCompletion>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, completed_at, local_date, precision, source, path, task_text,
                    done_date, outbox_id, voided_at, detected_at
             FROM task_completions
             WHERE (?1 IS NULL OR completed_at >= ?1)
             ORDER BY completed_at ASC, id ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![since, limit], read_completion)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

fn apply_note_on(conn: &Connection, note: &NoteApply<'_>) -> Result<()> {
    for draft in note.voids {
        void_times(
            conn,
            note.path,
            &draft.task_text,
            draft.count,
            note.detected_at,
        )?;
    }
    for draft in note.completions {
        insert_draft(conn, note.path, draft, note.detected_at, note.zone)?;
    }
    replace_snapshot(conn, note.path, note.counts)?;
    save_fingerprint(conn, note.path, note.fingerprint)?;
    Ok(())
}

fn insert_draft(
    conn: &Connection,
    path: &str,
    draft: &CompletionDraft,
    detected_at: DateTime<Utc>,
    zone: &str,
) -> Result<()> {
    for _ in 0..draft.count {
        let hint = hint_for(conn, path, draft, detected_at)?;
        let timing = completion_timing(
            detected_at,
            zone,
            draft.done_date,
            draft.prior_closed,
            hint.as_ref(),
        );
        insert_completion_on(
            conn,
            &row_from_timing(path, &draft.task_text, detected_at, timing),
        )?;
    }
    Ok(())
}

fn hint_for(
    conn: &Connection,
    path: &str,
    draft: &CompletionDraft,
    detected_at: DateTime<Utc>,
) -> Result<Option<DoneHint>> {
    let Some(text) = draft.hint_text.as_deref() else {
        return Ok(None);
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    find_done_hint_on(conn, path, text, detected_at)
}

fn row_from_timing(
    path: &str,
    task_text: &str,
    detected_at: DateTime<Utc>,
    timing: CompletionTiming,
) -> NewCompletion {
    NewCompletion {
        completed_at: timing.completed_at,
        local_date: timing.local_date,
        precision: timing.precision.to_string(),
        source: timing.source.to_string(),
        path: path.to_string(),
        task_text: task_text.to_string(),
        done_date: timing.done_date,
        outbox_id: timing.outbox_id,
        detected_at,
    }
}

fn void_times(
    conn: &Connection,
    path: &str,
    task_text: &str,
    count: u32,
    detected_at: DateTime<Utc>,
) -> Result<()> {
    for _ in 0..count {
        void_newest(conn, path, task_text, detected_at)?;
    }
    Ok(())
}

fn void_newest(
    conn: &Connection,
    path: &str,
    task_text: &str,
    detected_at: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "UPDATE task_completions
         SET voided_at = ?1
         WHERE id = (
             SELECT id FROM task_completions
             WHERE path = ?2 AND task_text = ?3 AND voided_at IS NULL
             ORDER BY completed_at DESC, id DESC
             LIMIT 1
         )",
        params![stamp(detected_at), path, task_text],
    )?;
    Ok(())
}

fn insert_completion_on(conn: &Connection, row: &NewCompletion) -> Result<bool> {
    let changed = conn.execute(
        "INSERT OR IGNORE INTO task_completions(
            completed_at, local_date, precision, source, path, task_text,
            done_date, outbox_id, voided_at, detected_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9)",
        params![
            stamp(row.completed_at),
            row.local_date,
            row.precision,
            row.source,
            row.path,
            row.task_text,
            row.done_date,
            row.outbox_id,
            stamp(row.detected_at),
        ],
    )?;
    Ok(changed == 1)
}

fn find_done_hint_on(
    conn: &Connection,
    path: &str,
    text: &str,
    detected_at: DateTime<Utc>,
) -> Result<Option<DoneHint>> {
    let cutoff = hint_cutoff(detected_at);
    let mut stmt = conn.prepare(
        "SELECT id, source_json, resolved_at
         FROM outbox
         WHERE resolution = ?1
           AND resolved_at IS NOT NULL
           AND resolved_at >= ?2
           AND id NOT IN (
               SELECT outbox_id FROM task_completions WHERE outbox_id IS NOT NULL
           )
         ORDER BY resolved_at DESC, id DESC",
    )?;
    let rows = stmt.query_map(params![SOURCE_LIBERADO_RESOLUTION, cutoff], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (id, source, resolved) = row?;
        if !hint_matches(source.as_deref(), path, text) {
            continue;
        }
        return Ok(Some(DoneHint {
            outbox_id: id,
            resolved_at: parse_stamp(&resolved)?,
        }));
    }
    Ok(None)
}

const SOURCE_LIBERADO_RESOLUTION: &str = crate::resolution::RES_COMPLETED;

fn hint_cutoff(detected_at: DateTime<Utc>) -> String {
    let at = detected_at
        .checked_sub_signed(Duration::minutes(DONE_HINT_MINUTES))
        .unwrap_or(detected_at);
    stamp(at)
}

fn hint_matches(source_json: Option<&str>, path: &str, text: &str) -> bool {
    let Some(source) = OutboxSource::decode(source_json.map(str::to_string)) else {
        return false;
    };
    match source {
        OutboxSource::VaultNote {
            path: note,
            text: note_text,
            ..
        } => note == path && note_text.trim() == text.trim(),
        _ => false,
    }
}

fn snapshot_counts_on(conn: &Connection, path: &str) -> Result<BTreeMap<String, TaskCounts>> {
    let mut stmt = conn
        .prepare("SELECT task_text, open_count, closed_count FROM task_snapshot WHERE path = ?1")?;
    let rows = stmt.query_map([path], |row| {
        Ok((
            row.get::<_, String>(0)?,
            TaskCounts {
                open: row.get(1)?,
                closed: row.get(2)?,
            },
        ))
    })?;
    let mut counts = BTreeMap::new();
    for row in rows {
        let (text, counts_row) = row?;
        counts.insert(text, counts_row);
    }
    Ok(counts)
}

fn replace_snapshot(
    conn: &Connection,
    path: &str,
    counts: &BTreeMap<String, TaskCounts>,
) -> Result<()> {
    conn.execute("DELETE FROM task_snapshot WHERE path = ?1", [path])?;
    for (task_text, counts) in counts {
        if counts.open == 0 && counts.closed == 0 {
            continue;
        }
        conn.execute(
            "INSERT INTO task_snapshot(path, task_text, open_count, closed_count)
             VALUES (?1, ?2, ?3, ?4)",
            params![path, task_text, counts.open, counts.closed],
        )?;
    }
    Ok(())
}

fn save_fingerprint(conn: &Connection, path: &str, fingerprint: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO task_file_state(path, fingerprint) VALUES (?1, ?2)
         ON CONFLICT(path) DO UPDATE SET fingerprint = excluded.fingerprint",
        params![path, fingerprint],
    )?;
    Ok(())
}

fn read_completion(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskCompletion> {
    Ok(TaskCompletion {
        id: row.get(0)?,
        completed_at: row.get(1)?,
        local_date: row.get(2)?,
        precision: row.get(3)?,
        source: row.get(4)?,
        path: row.get(5)?,
        task_text: row.get(6)?,
        done_date: row.get(7)?,
        outbox_id: row.get(8)?,
        voided_at: row.get(9)?,
        detected_at: row.get(10)?,
    })
}

pub fn content_fingerprint(text: &str) -> String {
    use sha2::{Digest, Sha256};
    hex_encode(&Sha256::digest(text.as_bytes()))
}

#[cfg(test)]
#[path = "completions_tests.rs"]
mod completions_tests;

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Tap row written when Done targets a recurring line.
pub fn liberado_tap_completion(
    path: &str,
    task_text: &str,
    outbox_id: i64,
    at: DateTime<Utc>,
    zone: &str,
) -> NewCompletion {
    NewCompletion {
        completed_at: at,
        local_date: crate::completion_time::local_date_of(at, zone),
        precision: PRECISION_TAP.to_string(),
        source: SOURCE_LIBERADO.to_string(),
        path: path.to_string(),
        task_text: task_text.to_string(),
        done_date: None,
        outbox_id: Some(outbox_id),
        detected_at: at,
    }
}
