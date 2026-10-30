use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

use crate::error::Result;
use crate::store::Outbox;
use crate::time::{parse_stamp, stamp};

impl Outbox {
    /// Insert `now` when the name is new and return the stored instant.
    /// A first call therefore has an empty catch-up window.
    pub fn ensure_cursor(&self, name: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
        let when = stamp(now);
        let conn = self.lock();
        conn.execute(
            "INSERT INTO schedule_cursor(name, last_fire_utc) VALUES (?1, ?2)
             ON CONFLICT(name) DO NOTHING",
            params![name, when],
        )?;
        let stored: String = conn.query_row(
            "SELECT last_fire_utc FROM schedule_cursor WHERE name = ?1",
            [name],
            |row| row.get(0),
        )?;
        parse_stamp(&stored)
    }

    /// Move the cursor forward after an enqueue commits. An older instant is ignored.
    pub fn record_cursor(&self, name: &str, fire: DateTime<Utc>) -> Result<()> {
        let when = stamp(fire);
        self.lock().execute(
            "INSERT INTO schedule_cursor(name, last_fire_utc) VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET last_fire_utc = excluded.last_fire_utc
             WHERE excluded.last_fire_utc > schedule_cursor.last_fire_utc",
            params![name, when],
        )?;
        Ok(())
    }

    pub fn cursor(&self, name: &str) -> Result<Option<DateTime<Utc>>> {
        let stored: Option<String> = self
            .lock()
            .query_row(
                "SELECT last_fire_utc FROM schedule_cursor WHERE name = ?1",
                [name],
                |row| row.get(0),
            )
            .optional()?;
        stored.map(|text| parse_stamp(&text)).transpose()
    }
}
