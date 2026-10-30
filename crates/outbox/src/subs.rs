use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::error::{Error, Result};
use crate::rows::host_label;
use crate::store::Outbox;
use crate::time::stamp;

const MAX_SUBSCRIPTIONS: i64 = 32;
const MAX_ENDPOINT: usize = 2000;
const MAX_LABEL: usize = 80;

#[derive(Debug, Clone)]
pub struct NewSubscription {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubscriptionView {
    pub id: i64,
    pub label: String,
    pub host: String,
}

#[derive(Debug, Clone)]
pub struct SubscriptionKeys {
    pub id: i64,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

impl Outbox {
    pub fn upsert_subscription(
        &self,
        sub: &NewSubscription,
        now: DateTime<Utc>,
    ) -> Result<i64> {
        check_subscription(sub)?;
        let label = label_of(&sub.label);
        let conn = self.lock();
        let existing: i64 = conn.query_row("SELECT COUNT(*) FROM push_subscriptions", [], |row| {
            row.get(0)
        })?;
        let already: Option<i64> = conn
            .query_row(
                "SELECT id FROM push_subscriptions WHERE endpoint = ?1",
                [&sub.endpoint],
                |row| row.get(0),
            )
            .optional()?;
        if already.is_none() && existing >= MAX_SUBSCRIPTIONS {
            return Err(Error::Invalid("at most 32 push subscriptions".into()));
        }
        conn.execute(
            "INSERT INTO push_subscriptions(endpoint, p256dh, auth, label, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(endpoint) DO UPDATE SET
                p256dh = excluded.p256dh,
                auth = excluded.auth,
                label = excluded.label",
            params![sub.endpoint, sub.p256dh, sub.auth, label, stamp(now)],
        )?;
        conn.query_row(
            "SELECT id FROM push_subscriptions WHERE endpoint = ?1",
            [&sub.endpoint],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    pub fn delete_subscription(&self, id: i64) -> Result<bool> {
        let changed = self
            .lock()
            .execute("DELETE FROM push_subscriptions WHERE id = ?1", [id])?;
        Ok(changed == 1)
    }

    pub fn delete_subscription_endpoint(&self, endpoint: &str) -> Result<bool> {
        let changed = self
            .lock()
            .execute("DELETE FROM push_subscriptions WHERE endpoint = ?1", [endpoint])?;
        Ok(changed == 1)
    }

    pub fn list_subscriptions(&self) -> Result<Vec<SubscriptionView>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT id, label, endpoint FROM push_subscriptions ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            let endpoint: String = row.get(2)?;
            Ok(SubscriptionView {
                id: row.get(0)?,
                label: row.get(1)?,
                host: host_label(&endpoint),
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn subscription_ids(&self) -> Result<Vec<i64>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT id FROM push_subscriptions ORDER BY id")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn subscription_keys(&self, id: i64) -> Result<Option<SubscriptionKeys>> {
        self.lock()
            .query_row(
                "SELECT id, endpoint, p256dh, auth FROM push_subscriptions WHERE id = ?1",
                [id],
                |row| {
                    Ok(SubscriptionKeys {
                        id: row.get(0)?,
                        endpoint: row.get(1)?,
                        p256dh: row.get(2)?,
                        auth: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn subscription_count(&self) -> Result<u32> {
        let count: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM push_subscriptions",
            [],
            |row| row.get(0),
        )?;
        Ok(u32::try_from(count).unwrap_or(0))
    }
}

fn check_subscription(sub: &NewSubscription) -> Result<()> {
    if sub.endpoint.len() > MAX_ENDPOINT || !endpoint_ok(&sub.endpoint) {
        return Err(Error::Invalid("push endpoint is not a usable URL".into()));
    }
    let public = decode(&sub.p256dh).map_err(|_| Error::Invalid("subscription key".into()))?;
    if public.len() != 65 || public.first() != Some(&0x04) {
        return Err(Error::Invalid(
            "subscription key is not a 65-byte P-256 key".into(),
        ));
    }
    let auth = decode(&sub.auth).map_err(|_| Error::Invalid("subscription auth".into()))?;
    if auth.len() != 16 {
        return Err(Error::Invalid("subscription auth is not 16 bytes".into()));
    }
    Ok(())
}

fn endpoint_ok(endpoint: &str) -> bool {
    let Some((scheme, rest)) = endpoint.split_once("://") else {
        return false;
    };
    matches!(scheme, "https" | "http") && rest.contains('/')
}

fn decode(text: &str) -> std::result::Result<Vec<u8>, ()> {
    URL_SAFE_NO_PAD
        .decode(text.trim().trim_end_matches('='))
        .map_err(|_| ())
}

fn label_of(label: &str) -> String {
    let trimmed = label.trim();
    let source = if trimmed.is_empty() { "phone" } else { trimmed };
    source.chars().take(MAX_LABEL).collect()
}
