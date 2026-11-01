//! Accounts, passkeys, and browser auth sessions in `liberado.db`.

use std::time::Duration;

use chrono::{DateTime, Utc};
use rand::RngCore;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::store::Outbox;
use crate::time::stamp;

pub const SESSION_ABSOLUTE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub const SESSION_IDLE: Duration = Duration::from_secs(14 * 24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub public_id: String,
    pub account_id: i64,
    pub csrf: String,
    pub method: String,
    pub created_at: String,
    pub last_seen_at: String,
    pub expires_at: String,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PasskeyRow {
    pub id: i64,
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PasskeyCredential {
    pub id: i64,
    pub credential_id: Vec<u8>,
    pub passkey_json: String,
    pub sign_count: u32,
}

pub fn hash_session_id(raw_id: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(raw_id.as_bytes());
    hasher.finalize().into()
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

impl Outbox {
    pub fn passkey_count(&self, account_id: i64) -> Result<i64> {
        self.lock()
            .query_row(
                "SELECT COUNT(*) FROM passkeys WHERE account_id = ?1",
                [account_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn list_passkeys(&self, account_id: i64) -> Result<Vec<PasskeyRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, created_at, last_used_at FROM passkeys
             WHERE account_id = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map([account_id], |row| {
                Ok(PasskeyRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    created_at: row.get(2)?,
                    last_used_at: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn delete_passkey(&self, account_id: i64, id: i64) -> Result<bool> {
        let changed = self.lock().execute(
            "DELETE FROM passkeys WHERE id = ?1 AND account_id = ?2",
            params![id, account_id],
        )?;
        Ok(changed == 1)
    }

    pub fn rename_passkey(&self, account_id: i64, id: i64, name: &str) -> Result<bool> {
        let trimmed = name.trim();
        if trimmed.is_empty() || trimmed.len() > 80 {
            return Err(Error::Invalid(
                "passkey name must be 1–80 characters".into(),
            ));
        }
        let changed = self.lock().execute(
            "UPDATE passkeys SET name = ?3 WHERE id = ?1 AND account_id = ?2",
            params![id, account_id, trimmed],
        )?;
        Ok(changed == 1)
    }

    pub fn insert_passkey(
        &self,
        account_id: i64,
        credential_id: &[u8],
        passkey_json: &str,
        name: &str,
        now: DateTime<Utc>,
    ) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO passkeys(account_id, credential_id, passkey_json, name, created_at, sign_count)
             VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            params![
                account_id,
                credential_id,
                passkey_json,
                name.trim(),
                stamp(now)
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn passkey_by_credential_id(
        &self,
        credential_id: &[u8],
    ) -> Result<Option<PasskeyCredential>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, credential_id, passkey_json, sign_count FROM passkeys WHERE credential_id = ?1",
            [credential_id],
            |row| {
                Ok(PasskeyCredential {
                    id: row.get(0)?,
                    credential_id: row.get(1)?,
                    passkey_json: row.get(2)?,
                    sign_count: row.get::<_, i64>(3)? as u32,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn update_passkey_after_auth(
        &self,
        id: i64,
        passkey_json: &str,
        sign_count: u32,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.lock().execute(
            "UPDATE passkeys SET passkey_json = ?2, sign_count = ?3, last_used_at = ?4 WHERE id = ?1",
            params![id, passkey_json, i64::from(sign_count), stamp(now)],
        )?;
        Ok(())
    }

    pub fn owner_webauthn_user_id(&self) -> Result<Vec<u8>> {
        self.lock()
            .query_row(
                "SELECT webauthn_user_id FROM accounts WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn passkey_json_values(&self, account_id: i64) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT passkey_json FROM passkeys WHERE account_id = ?1 ORDER BY id")?;
        let rows = stmt
            .query_map([account_id], |row| row.get(0))?
            .collect::<std::result::Result<Vec<String>, _>>()?;
        Ok(rows)
    }

    pub fn create_auth_session(
        &self,
        account_id: i64,
        method: &str,
        user_agent: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(String, String)> {
        let raw_id = random_hex(32);
        let csrf = random_hex(32);
        let public_id = random_hex(8);
        let id_hash = hash_session_id(&raw_id);
        let expires = now
            + chrono::Duration::from_std(SESSION_ABSOLUTE)
                .unwrap_or_else(|_| chrono::Duration::days(30));
        let conn = self.lock();
        prune_sessions(&conn, now)?;
        conn.execute(
            "INSERT INTO auth_sessions(
                id_hash, public_id, account_id, csrf, method,
                created_at, last_seen_at, expires_at, user_agent)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8)",
            params![
                id_hash.as_slice(),
                public_id,
                account_id,
                csrf,
                method,
                stamp(now),
                stamp(expires),
                user_agent
            ],
        )?;
        Ok((raw_id, csrf))
    }

    pub fn touch_auth_session(&self, raw_id: &str, now: DateTime<Utc>) -> Option<SessionRecord> {
        let id_hash = hash_session_id(raw_id);
        let conn = self.lock();
        if prune_sessions(&conn, now).is_err() {
            return None;
        }
        let record = match conn
            .query_row(
                "SELECT public_id, account_id, csrf, method, created_at, last_seen_at, expires_at, user_agent
                 FROM auth_sessions WHERE id_hash = ?1",
                [id_hash.as_slice()],
                |row| {
                    Ok(SessionRecord {
                        public_id: row.get(0)?,
                        account_id: row.get(1)?,
                        csrf: row.get(2)?,
                        method: row.get(3)?,
                        created_at: row.get(4)?,
                        last_seen_at: row.get(5)?,
                        expires_at: row.get(6)?,
                        user_agent: row.get(7)?,
                    })
                },
            )
            .optional()
        {
            Ok(Some(r)) => r,
            _ => return None,
        };
        let last = parse_time(&record.last_seen_at)?;
        if now.signed_duration_since(last) > chrono::Duration::from_std(SESSION_IDLE).ok()? {
            let _ = conn.execute(
                "DELETE FROM auth_sessions WHERE id_hash = ?1",
                [id_hash.as_slice()],
            );
            return None;
        }
        let expires = parse_time(&record.expires_at)?;
        if now >= expires {
            let _ = conn.execute(
                "DELETE FROM auth_sessions WHERE id_hash = ?1",
                [id_hash.as_slice()],
            );
            return None;
        }
        let _ = conn.execute(
            "UPDATE auth_sessions SET last_seen_at = ?2 WHERE id_hash = ?1",
            params![id_hash.as_slice(), stamp(now)],
        );
        Some(record)
    }

    pub fn revoke_auth_session(&self, raw_id: &str) -> Result<()> {
        let id_hash = hash_session_id(raw_id);
        self.lock().execute(
            "DELETE FROM auth_sessions WHERE id_hash = ?1",
            [id_hash.as_slice()],
        )?;
        Ok(())
    }

    pub fn revoke_auth_session_public(&self, account_id: i64, public_id: &str) -> Result<bool> {
        let changed = self.lock().execute(
            "DELETE FROM auth_sessions WHERE public_id = ?1 AND account_id = ?2",
            params![public_id, account_id],
        )?;
        Ok(changed == 1)
    }

    pub fn list_auth_sessions(&self, account_id: i64) -> Result<Vec<SessionRecord>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT public_id, account_id, csrf, method, created_at, last_seen_at, expires_at, user_agent
             FROM auth_sessions WHERE account_id = ?1 ORDER BY last_seen_at DESC",
        )?;
        let rows = stmt
            .query_map([account_id], |row| {
                Ok(SessionRecord {
                    public_id: row.get(0)?,
                    account_id: row.get(1)?,
                    csrf: row.get(2)?,
                    method: row.get(3)?,
                    created_at: row.get(4)?,
                    last_seen_at: row.get(5)?,
                    expires_at: row.get(6)?,
                    user_agent: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn prune_sessions(conn: &rusqlite::Connection, now: DateTime<Utc>) -> Result<()> {
    let idle_cutoff = now
        - chrono::Duration::from_std(SESSION_IDLE).unwrap_or_else(|_| chrono::Duration::days(14));
    conn.execute(
        "DELETE FROM auth_sessions WHERE expires_at < ?1 OR last_seen_at < ?2",
        params![stamp(now), stamp(idle_cutoff)],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Outbox;

    #[test]
    fn raw_session_id_is_never_stored_in_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::open(&dir.path().join("liberado.db")).unwrap();
        let now = Utc::now();
        let (raw, _) = outbox.create_auth_session(1, "token", None, now).unwrap();
        let bytes = std::fs::read(dir.path().join("liberado.db")).unwrap();
        assert!(
            !bytes.windows(raw.len()).any(|w| w == raw.as_bytes()),
            "database file must not contain the raw session id"
        );
    }

    #[test]
    fn session_survives_reopen_on_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("liberado.db");
        let (raw, csrf_expected);
        {
            let outbox = Outbox::open(&path).unwrap();
            let now = Utc::now();
            (raw, csrf_expected) = outbox
                .create_auth_session(1, "passkey", Some("test-ua"), now)
                .unwrap();
        }
        {
            let outbox = Outbox::open(&path).unwrap();
            let touched = outbox
                .touch_auth_session(&raw, Utc::now())
                .expect("session should load after reopen");
            assert_eq!(touched.csrf, csrf_expected);
            assert_eq!(touched.method, "passkey");
        }
    }

    #[test]
    fn revoke_and_expiry_drop_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::open(&dir.path().join("liberado.db")).unwrap();
        let now = Utc::now();
        let (raw, _) = outbox.create_auth_session(1, "token", None, now).unwrap();
        outbox.revoke_auth_session(&raw).unwrap();
        assert!(outbox.touch_auth_session(&raw, now).is_none());
    }
}
