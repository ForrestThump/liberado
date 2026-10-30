use chrono::{DateTime, Duration, Utc};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::ack::{hash_token, new_token};
use crate::error::Result;
use crate::plan::OpenRow;
use crate::store::Outbox;
use crate::time::{parse_stamp, stamp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewReminder {
    pub idempotency_key: String,
    pub channel: String,
    pub target: String,
    pub title: String,
    pub body: String,
    pub due_at: DateTime<Utc>,
    pub catch_up: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueOutcome {
    Inserted(i64),
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VisibleReminder {
    pub id: i64,
    pub channel: String,
    pub target_host: String,
    pub title: String,
    pub state: String,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OutboxCounts {
    pub pending: u64,
    pub failed: u64,
    pub dead: u64,
    pub oldest_pending_secs: Option<u64>,
}

impl Outbox {
    pub fn enqueue(&self, row: &NewReminder, now: DateTime<Utc>) -> Result<EnqueueOutcome> {
        let token = new_token();
        let hash = hash_token(&token);
        let when = stamp(now);
        let due = stamp(row.due_at);
        let conn = self.lock();
        let changed = conn.execute(
            "INSERT OR IGNORE INTO outbox (
                idempotency_key, channel, target, title, body, state, attempts,
                next_attempt_at, created_at, due_at, catch_up, ack_token_hash, ack_token
            ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 0, ?6, ?6, ?7, ?8, ?9, ?10)",
            params![
                row.idempotency_key,
                row.channel,
                row.target,
                clip(&row.title, 180),
                clip(&row.body, 1800),
                when,
                due,
                if row.catch_up { 1 } else { 0 },
                hash.as_slice(),
                token,
            ],
        )?;
        if changed == 0 {
            return Ok(EnqueueOutcome::Duplicate);
        }
        Ok(EnqueueOutcome::Inserted(conn.last_insert_rowid()))
    }

    pub fn due_rows(&self, now: DateTime<Utc>, max_catchup: u32) -> Result<Vec<OpenRow>> {
        let when = stamp(now);
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, channel, target, title, body, catch_up, due_at, ack_token
             FROM outbox
             WHERE ack_token IS NOT NULL
               AND ((state IN ('pending', 'failed') AND next_attempt_at <= ?1)
                 OR (state = 'delivering' AND lease_until <= ?1))
             ORDER BY due_at ASC",
        )?;
        let rows = stmt.query_map(params![when], read_open)?;
        let mut normal = Vec::new();
        let mut catch_up = Vec::new();
        for row in rows {
            let row = row?;
            if row.catch_up {
                if catch_up.len() < max_catchup as usize {
                    catch_up.push(row);
                }
            } else {
                normal.push(row);
            }
        }
        normal.append(&mut catch_up);
        Ok(normal)
    }

    pub fn lease(&self, ids: &[i64], now: DateTime<Utc>) -> Result<()> {
        let until = stamp(now + Duration::minutes(2));
        let conn = self.lock();
        let tx = conn.unchecked_transaction()?;
        for id in ids {
            tx.execute(
                "UPDATE outbox
                 SET state = 'delivering', attempts = attempts + 1, lease_until = ?1
                 WHERE id = ?2",
                params![until, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn release(&self, ids: &[i64], message: &str) -> Result<()> {
        let conn = self.lock();
        for id in ids {
            conn.execute(
                "UPDATE outbox
                 SET state = 'pending',
                     attempts = CASE WHEN attempts > 0 THEN attempts - 1 ELSE 0 END,
                     lease_until = NULL,
                     last_error = ?1
                 WHERE id = ?2",
                params![clip(message, 300), id],
            )?;
        }
        Ok(())
    }

    pub fn note(&self, ids: &[i64], message: &str) -> Result<()> {
        let conn = self.lock();
        for id in ids {
            conn.execute(
                "UPDATE outbox SET last_error = ?1 WHERE id = ?2",
                params![clip(message, 300), id],
            )?;
        }
        Ok(())
    }

    pub fn mark_accepted(&self, ids: &[i64], now: DateTime<Utc>) -> Result<()> {
        let when = stamp(now);
        let conn = self.lock();
        let tx = conn.unchecked_transaction()?;
        for id in ids {
            tx.execute(
                "UPDATE outbox
                 SET state = 'delivered', accepted_at = ?1, finished_at = ?1,
                     lease_until = NULL, ack_token = NULL, last_error = NULL
                 WHERE id = ?2",
                params![when, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn mark_retry(&self, ids: &[i64], message: &str, now: DateTime<Utc>, max_attempts: i64) -> Result<()> {
        let conn = self.lock();
        let tx = conn.unchecked_transaction()?;
        for id in ids {
            let attempts: i64 =
                tx.query_row("SELECT attempts FROM outbox WHERE id = ?1", [id], |row| {
                    row.get(0)
                })?;
            if attempts >= max_attempts {
                finish_dead(&tx, *id, message, now)?;
            } else {
                let next = stamp(now + Duration::seconds(backoff_secs(attempts)));
                tx.execute(
                    "UPDATE outbox
                     SET state = 'failed', next_attempt_at = ?1, lease_until = NULL, last_error = ?2
                     WHERE id = ?3",
                    params![next, clip(message, 300), id],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn mark_dead(&self, ids: &[i64], message: &str, now: DateTime<Utc>) -> Result<()> {
        let conn = self.lock();
        let tx = conn.unchecked_transaction()?;
        for id in ids {
            finish_dead(&tx, *id, message, now)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn retry(&self, id: i64, now: DateTime<Utc>) -> Result<bool> {
        let when = stamp(now);
        let changed = self.lock().execute(
            "UPDATE outbox
             SET state = 'pending', attempts = 0, next_attempt_at = ?1,
                 lease_until = NULL, finished_at = NULL, last_error = NULL
             WHERE id = ?2 AND state IN ('dead', 'failed')",
            params![when, id],
        )?;
        Ok(changed == 1)
    }

    pub fn counts(&self, now: DateTime<Utc>) -> Result<OutboxCounts> {
        let conn = self.lock();
        let pending = count_states(&conn, &["pending", "delivering"])?;
        let failed = count_states(&conn, &["failed"])?;
        let dead = count_states(&conn, &["dead"])?;
        let oldest: Option<String> = conn
            .query_row(
                "SELECT MIN(created_at) FROM outbox WHERE state IN ('pending', 'delivering')",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let oldest_pending_secs = match oldest {
            Some(text) => Some(age_secs(&text, now)?),
            None => None,
        };
        Ok(OutboxCounts {
            pending,
            failed,
            dead,
            oldest_pending_secs,
        })
    }

    pub fn list_visible(&self, limit: u32) -> Result<Vec<VisibleReminder>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, channel, target, title, state, attempts, last_error, created_at
             FROM outbox
             WHERE state IN ('pending', 'delivering', 'failed', 'dead')
             ORDER BY created_at ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(VisibleReminder {
                id: row.get(0)?,
                channel: row.get(1)?,
                target_host: host_label(&row.get::<_, String>(2)?),
                title: row.get(3)?,
                state: row.get(4)?,
                attempts: row.get(5)?,
                last_error: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn prune(&self, now: DateTime<Utc>) -> Result<usize> {
        let cutoff = stamp(now - Duration::days(30));
        let changed = self.lock().execute(
            "DELETE FROM outbox
             WHERE state IN ('delivered', 'dead') AND finished_at IS NOT NULL AND finished_at < ?1",
            params![cutoff],
        )?;
        Ok(changed)
    }

    pub fn attempts_of(&self, id: i64) -> Result<i64> {
        self.lock()
            .query_row("SELECT attempts FROM outbox WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .map_err(Into::into)
    }

    pub fn state_of(&self, id: i64) -> Result<String> {
        self.lock()
            .query_row("SELECT state FROM outbox WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .map_err(Into::into)
    }

    pub fn ack(&self, id: i64, token: &str, now: DateTime<Utc>) -> Result<()> {
        let presented = hash_token(token);
        let conn = self.lock();
        let stored: Option<Vec<u8>> = conn
            .query_row(
                "SELECT ack_token_hash FROM outbox WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        let dummy = [0u8; 32];
        let candidate = stored.as_deref().unwrap_or(&dummy);
        if !crate::ack::constant_time_eq(&presented, candidate) || stored.is_none() {
            return Ok(());
        }
        conn.execute(
            "UPDATE outbox SET acked_at = ?1 WHERE id = ?2 AND acked_at IS NULL",
            params![stamp(now), id],
        )?;
        Ok(())
    }

    pub fn acked(&self, id: i64) -> Result<bool> {
        let value: Option<String> = self
            .lock()
            .query_row("SELECT acked_at FROM outbox WHERE id = ?1", [id], |row| {
                row.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten();
        Ok(value.is_some())
    }
}

fn finish_dead(
    tx: &rusqlite::Transaction<'_>,
    id: i64,
    message: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    tx.execute(
        "UPDATE outbox
         SET state = 'dead', finished_at = ?1, lease_until = NULL, ack_token = NULL, last_error = ?2
         WHERE id = ?3",
        params![stamp(now), clip(message, 300), id],
    )?;
    Ok(())
}

fn read_open(row: &rusqlite::Row<'_>) -> rusqlite::Result<OpenRow> {
    let due: String = row.get(6)?;
    let token: String = row.get(7)?;
    Ok(OpenRow {
        id: row.get(0)?,
        channel: row.get(1)?,
        target: row.get(2)?,
        title: row.get(3)?,
        body: row.get(4)?,
        catch_up: row.get::<_, i64>(5)? != 0,
        due_at: parse_stamp(&due).unwrap_or_else(|_| Utc::now()),
        token,
    })
}

fn count_states(conn: &rusqlite::Connection, states: &[&str]) -> Result<u64> {
    let mut total = 0u64;
    for state in states {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM outbox WHERE state = ?1",
            [*state],
            |row| row.get(0),
        )?;
        total += u64::try_from(count).unwrap_or(0);
    }
    Ok(total)
}

fn age_secs(created: &str, now: DateTime<Utc>) -> Result<u64> {
    let at = parse_stamp(created)?;
    Ok(now.signed_duration_since(at).num_seconds().max(0) as u64)
}

fn backoff_secs(attempts: i64) -> i64 {
    let shift = u32::try_from(attempts.saturating_sub(1).min(16)).unwrap_or(16);
    let cap = 15i64.saturating_mul(1i64 << shift).min(3600);
    let mut rng = rand::rngs::OsRng;
    rand::Rng::gen_range(&mut rng, 0..=cap)
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn host_label(target: &str) -> String {
    if let Some(rest) = target.split_once("://") {
        return rest
            .1
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("push")
            .to_string();
    }
    if target.is_empty() {
        return "unset".into();
    }
    target.to_string()
}
