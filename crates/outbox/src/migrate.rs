//! Schema version upgrades for `liberado.db`.

use rand::RngCore;
use rusqlite::Connection;

use crate::error::{Error, Result};
use crate::time::stamp;

pub const SCHEMA_VERSION: i64 = 2;

const V2_TABLES: &str = "
CREATE TABLE IF NOT EXISTS accounts (
    id INTEGER PRIMARY KEY,
    handle TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    webauthn_user_id BLOB NOT NULL UNIQUE,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS passkeys (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id),
    credential_id BLOB NOT NULL UNIQUE,
    passkey_json TEXT NOT NULL,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_used_at TEXT,
    sign_count INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS auth_sessions (
    id_hash BLOB PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE,
    account_id INTEGER NOT NULL REFERENCES accounts(id),
    csrf TEXT NOT NULL,
    method TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    user_agent TEXT
);
CREATE INDEX IF NOT EXISTS auth_sessions_account ON auth_sessions(account_id);
";

fn read_version(conn: &Connection) -> Result<i64> {
    conn.query_row(
        "SELECT value FROM schema_meta WHERE key = 'version'",
        [],
        |row| row.get::<_, String>(0),
    )
    .map(|s| s.parse().unwrap_or(1))
    .map_err(Into::into)
}

fn set_version(conn: &Connection, version: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO schema_meta(key, value) VALUES ('version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [version.to_string()],
    )?;
    Ok(())
}

fn seed_owner_account(conn: &Connection) -> Result<()> {
    let exists: i64 = conn.query_row("SELECT COUNT(*) FROM accounts WHERE id = 1", [], |r| {
        r.get(0)
    })?;
    if exists > 0 {
        return Ok(());
    }
    let mut user_id = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut user_id);
    let now = stamp(chrono::Utc::now());
    conn.execute(
        "INSERT INTO accounts(id, handle, display_name, webauthn_user_id, created_at)
         VALUES (1, 'owner', 'Owner', ?1, ?2)",
        rusqlite::params![user_id.as_slice(), now],
    )?;
    Ok(())
}

/// Apply pending migrations. Idempotent when already at [`SCHEMA_VERSION`].
pub fn migrate(conn: &Connection) -> Result<()> {
    let version = read_version(conn)?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    if version == 1 {
        conn.execute_batch(V2_TABLES)?;
        seed_owner_account(conn)?;
        set_version(conn, SCHEMA_VERSION)?;
    } else {
        return Err(Error::Invalid(format!(
            "unsupported liberado.db schema version {version}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use rusqlite::Connection;
    use tempfile::TempDir;

    use super::*;
    use crate::store::Outbox;

    const V1_SCHEMA: &str = "
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
";

    fn open_v1_with_rows(dir: &TempDir) -> Connection {
        let path = dir.path().join("liberado.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(V1_SCHEMA).unwrap();
        let now = stamp(Utc::now());
        conn.execute(
            "INSERT INTO outbox(idempotency_key, channel, target, title, body, state,
             next_attempt_at, created_at, due_at, ack_token_hash)
             VALUES ('k1', 'web_push', 'sub:1', 't', 'b', 'pending', ?1, ?1, ?1, x'00')",
            [now.as_str()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO push_subscriptions(endpoint, p256dh, auth, label, created_at)
             VALUES ('https://example.com/push', 'k', 'a', 'phone', ?1)",
            [now.as_str()],
        )
        .unwrap();
        conn
    }

    #[test]
    fn v1_database_migrates_to_v2_and_keeps_rows() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_v1_with_rows(&dir);
        migrate(&conn).unwrap();
        assert_eq!(read_version(&conn).unwrap(), SCHEMA_VERSION);
        let outbox_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get(0))
            .unwrap();
        assert_eq!(outbox_rows, 1);
        let sub_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM push_subscriptions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sub_rows, 1);
        let handle: String = conn
            .query_row("SELECT handle FROM accounts WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(handle, "owner");
    }

    #[test]
    fn migration_is_idempotent_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("liberado.db");
        {
            let conn = open_v1_with_rows(&dir);
            migrate(&conn).unwrap();
        }
        let outbox = Outbox::open(&path).unwrap();
        drop(outbox);
        let conn = Connection::open(&path).unwrap();
        assert_eq!(read_version(&conn).unwrap(), SCHEMA_VERSION);
        migrate(&conn).unwrap();
        let accounts: i64 = conn
            .query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(accounts, 1);
    }
}
