//! Single-row reads and owner resolution for the notification detail API.

use chrono::{DateTime, Duration, Utc};
use rusqlite::{OptionalExtension, Row, params};

use crate::error::Result;
use crate::resolution::{OwnerResolveResult, OwnerSnoozeResult, RES_SNOOZED};
use crate::rows::{NewReminder, insert_reminder};
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
    pub resolution: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
}

impl Outbox {
    pub fn fetch_record(&self, id: i64) -> Result<Option<OutboxRecord>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT id, channel, target, title, body, catch_up, due_at, kind, source_json, state, acked_at,
                        resolution, resolved_at
                 FROM outbox WHERE id = ?1",
                [id],
                read_outbox_record,
            )
            .optional()?;
        Ok(row)
    }

    pub fn due_at_by_idempotency(&self, key: &str) -> Result<Option<DateTime<Utc>>> {
        let conn = self.lock();
        let due: Option<String> = conn
            .query_row(
                "SELECT due_at FROM outbox WHERE idempotency_key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()?;
        due.map(|text| parse_stamp(&text)).transpose()
    }

    /// Claim owner resolution when the row is not yet resolved.
    pub fn try_resolve(
        &self,
        id: i64,
        resolution: &str,
        now: DateTime<Utc>,
    ) -> Result<OwnerResolveResult> {
        let when = stamp(now);
        let changed = self.lock().execute(
            "UPDATE outbox
             SET resolved_at = ?1, resolution = ?2, acked_at = COALESCE(acked_at, ?1)
             WHERE id = ?3 AND resolved_at IS NULL",
            params![when, resolution, id],
        )?;
        if changed == 1 {
            return Ok(OwnerResolveResult::Applied);
        }
        let conn = self.lock();
        let existing: Option<String> = conn
            .query_row(
                "SELECT resolution FROM outbox WHERE id = ?1 AND resolved_at IS NOT NULL",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        match existing {
            Some(value) => Ok(OwnerResolveResult::AlreadyResolved(value)),
            None => Ok(OwnerResolveResult::NotFound),
        }
    }

    /// Undo a resolution claim (e.g. vault write failed after claiming completed).
    pub fn clear_resolution(&self, id: i64, expected: &str) -> Result<bool> {
        let changed = self.lock().execute(
            "UPDATE outbox
             SET resolved_at = NULL, resolution = NULL
             WHERE id = ?1 AND resolution = ?2",
            params![id, expected],
        )?;
        Ok(changed == 1)
    }

    /// Snooze: claim resolution on the original row and enqueue one follow-up.
    pub fn owner_snooze(
        &self,
        id: i64,
        minutes: i64,
        now: DateTime<Utc>,
    ) -> Result<OwnerSnoozeResult> {
        let conn = self.lock();
        let record = conn
            .query_row(
                "SELECT channel, target, title, body, kind, source_json
                 FROM outbox WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        crate::kind::parse_stored(row.get::<_, Option<String>>(4)?.as_deref())
                            .to_string(),
                        OutboxSource::decode(row.get(5)?),
                    ))
                },
            )
            .optional()?;
        let Some((channel, target, title, body, kind, source)) = record else {
            return Ok(OwnerSnoozeResult::NotFound);
        };

        let tx = conn.unchecked_transaction()?;
        let when = stamp(now);
        let changed = tx.execute(
            "UPDATE outbox
             SET resolved_at = ?1, resolution = ?2, acked_at = COALESCE(acked_at, ?1)
             WHERE id = ?3 AND resolved_at IS NULL",
            params![when, RES_SNOOZED, id],
        )?;
        if changed == 0 {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT resolution FROM outbox WHERE id = ?1 AND resolved_at IS NOT NULL",
                    [id],
                    |row| row.get(0),
                )
                .optional()?;
            return Ok(match existing {
                Some(value) => OwnerSnoozeResult::AlreadyResolved(value),
                None => OwnerSnoozeResult::NotFound,
            });
        }

        let due = now + Duration::minutes(minutes);
        let follow_title = snooze_follow_up_title(&title);
        let key = format!("snooze/{id}");
        let reminder = NewReminder {
            idempotency_key: key.clone(),
            channel,
            target,
            title: follow_title,
            body,
            due_at: due,
            catch_up: false,
            kind: Some(kind),
            source,
        };
        let outcome = insert_reminder(&tx, &reminder, now)?;
        tx.commit()?;
        let follow_up_id = match outcome {
            crate::rows::EnqueueOutcome::Inserted(id) => id,
            crate::rows::EnqueueOutcome::Duplicate => conn.query_row(
                "SELECT id FROM outbox WHERE idempotency_key = ?1",
                [key.as_str()],
                |row| row.get(0),
            )?,
        };
        Ok(OwnerSnoozeResult::Applied { follow_up_id })
    }
}

pub fn snooze_follow_up_title(base: &str) -> String {
    const PREFIX: &str = "Snoozed: ";
    if base.starts_with(PREFIX) {
        base.to_string()
    } else {
        format!("{PREFIX}{base}")
    }
}

fn read_outbox_record(row: &Row<'_>) -> rusqlite::Result<OutboxRecord> {
    let due: String = row.get(6)?;
    let resolved: Option<String> = row.get(12)?;
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
        resolution: row.get(11)?,
        resolved_at: resolved
            .as_deref()
            .map(parse_stamp)
            .transpose()
            .ok()
            .flatten(),
    })
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use tempfile::NamedTempFile;

    use super::*;
    use crate::resolution::RES_DISMISSED;
    use crate::rows::{EnqueueOutcome, NewReminder};

    fn open_temp() -> (Outbox, std::path::PathBuf) {
        let file = NamedTempFile::new().unwrap();
        let path = file.into_temp_path().keep().unwrap();
        (Outbox::open(&path).unwrap(), path)
    }

    #[test]
    fn future_due_enqueue_is_not_due_until_scheduled_time() {
        let (outbox, _path) = open_temp();
        let now = Utc::now();
        let due = now + Duration::minutes(15);
        outbox
            .enqueue(
                &NewReminder {
                    idempotency_key: "future".into(),
                    channel: "web_push".into(),
                    target: "sub:1".into(),
                    title: "later".into(),
                    body: "body".into(),
                    due_at: due,
                    catch_up: false,
                    kind: None,
                    source: None,
                },
                now,
            )
            .unwrap();
        assert!(outbox.due_rows(now, 10).unwrap().is_empty());
        let after = due + Duration::seconds(1);
        assert_eq!(outbox.due_rows(after, 10).unwrap().len(), 1);
    }

    #[test]
    fn owner_resolve_works_after_deliver_and_push_ack() {
        let (outbox, _path) = open_temp();
        let now = Utc::now();
        let EnqueueOutcome::Inserted(id) = outbox
            .enqueue(
                &NewReminder {
                    idempotency_key: "flow".into(),
                    channel: "web_push".into(),
                    target: "sub:1".into(),
                    title: "t".into(),
                    body: "b".into(),
                    due_at: now,
                    catch_up: false,
                    kind: None,
                    source: None,
                },
                now,
            )
            .unwrap()
        else {
            panic!("expected insert");
        };
        let token = outbox.due_rows(now, 10).unwrap()[0].token.clone();
        outbox.lease(&[id], now).unwrap();
        outbox.mark_accepted(&[id], now).unwrap();
        outbox.ack(id, &token, now).unwrap();
        assert!(outbox.acked(id).unwrap());
        assert_eq!(
            outbox.try_resolve(id, RES_DISMISSED, now).unwrap(),
            OwnerResolveResult::Applied
        );
        assert_eq!(
            outbox.try_resolve(id, RES_DISMISSED, now).unwrap(),
            OwnerResolveResult::AlreadyResolved(RES_DISMISSED.into())
        );
    }

    #[test]
    fn snoozed_follow_up_not_due_until_scheduled_time() {
        let (outbox, _path) = open_temp();
        let now = Utc::now();
        outbox
            .enqueue(
                &NewReminder {
                    idempotency_key: "orig".into(),
                    channel: "web_push".into(),
                    target: "sub:1".into(),
                    title: "Due now: x".into(),
                    body: "body".into(),
                    due_at: now,
                    catch_up: false,
                    kind: None,
                    source: None,
                },
                now,
            )
            .unwrap();
        match outbox.owner_snooze(1, 15, now).unwrap() {
            OwnerSnoozeResult::Applied { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
        assert!(outbox.due_rows(now, 10).unwrap().is_empty());
        let fire = now + Duration::minutes(15) + Duration::seconds(1);
        let rows = outbox.due_rows(fire, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Snoozed: Due now: x");
    }

    #[test]
    fn snooze_title_does_not_stack_prefix() {
        assert_eq!(
            snooze_follow_up_title("Snoozed: Due now: x"),
            "Snoozed: Due now: x"
        );
        assert_eq!(snooze_follow_up_title("Due now: x"), "Snoozed: Due now: x");
    }
}
