use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use crate::error::{Error, Result};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS schema_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT OR IGNORE INTO schema_meta(key, value) VALUES ('version', '1');
CREATE TABLE IF NOT EXISTS outbox (
    id INTEGER PRIMARY KEY,
    idempotency_key TEXT NOT NULL UNIQUE,
    channel TEXT NOT NULL,
    target TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    state TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT NOT NULL,
    lease_until TEXT,
    created_at TEXT NOT NULL,
    accepted_at TEXT,
    acked_at TEXT,
    finished_at TEXT,
    last_error TEXT,
    due_at TEXT NOT NULL,
    catch_up INTEGER NOT NULL DEFAULT 0,
    ack_token_hash BLOB NOT NULL,
    ack_token TEXT
);
CREATE TABLE IF NOT EXISTS push_subscriptions (
    id INTEGER PRIMARY KEY,
    endpoint TEXT NOT NULL UNIQUE,
    p256dh TEXT NOT NULL,
    auth TEXT NOT NULL,
    label TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS schedule_cursor (
    name TEXT PRIMARY KEY,
    last_fire_utc TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS outbox_due ON outbox(state, next_attempt_at);
";

pub struct Outbox {
    conn: Mutex<Connection>,
}

impl Outbox {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|err| Error::Invalid(err.to_string()))?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|err| err.into_inner())
    }
}
