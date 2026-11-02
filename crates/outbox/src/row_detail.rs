//! Single-row reads and owner dismiss for the notification detail API.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};

use crate::error::Result;
use crate::source::OutboxSource;
use crate::store::Outbox;
use crate::time::{parse_stamp, stamp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRecord {
    pub id: i64,
    pub channel: String,
    pub target: String,
    pub title: String,
    pub body: String,
    pub catch_up: bool,
    pub due_at: DateTime<Utc>,
    pub kind: String,
    pub source: Option<OutboxSource>,
    pub state: String,
    pub acked: bool,
}

impl Outbox {
    pub fn fetch_record(&self, id: i64) -> Result<Option<OutboxRecord>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT id, channel, target, title, body, catch_up, due_at, kind, source_json, state, acked_at
                 FROM outbox WHERE id = ?1",
                [id],
                read_outbox_record,
            )
            .optional()?;
        Ok(row)
    }

    /// Owner-authenticated dismiss without the push ack token.
    pub fn owner_ack(&self, id: i64, now: DateTime<Utc>) -> Result<bool> {
        let changed = self.lock().execute(
            "UPDATE outbox SET acked_at = ?1 WHERE id = ?2 AND acked_at IS NULL AND ack_token IS NOT NULL",
            params![stamp(now), id],
        )?;
        Ok(changed == 1)
    }
}

fn read_outbox_record(row: &Row<'_>) -> rusqlite::Result<OutboxRecord> {
    let due: String = row.get(6)?;
    Ok(OutboxRecord {
        id: row.get(0)?,
        channel: row.get(1)?,
        target: row.get(2)?,
        title: row.get(3)?,
        body: row.get(4)?,
        catch_up: row.get::<_, i64>(5)? != 0,
        due_at: parse_stamp(&due).unwrap_or_else(|_| Utc::now()),
        kind: crate::kind::parse_stored(row.get::<_, Option<String>>(7)?.as_deref()).to_string(),
        source: OutboxSource::decode(row.get(8)?),
        state: row.get(9)?,
        acked: row.get::<_, Option<String>>(10)?.is_some(),
    })
}
