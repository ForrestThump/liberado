//! Schema version upgrades for `liberado.db`.

use rand::RngCore;
use rusqlite::Connection;

use crate::error::{Error, Result};
use crate::time::stamp;

pub const SCHEMA_VERSION: i64 = 5;

const V5_TABLES: &str = "
CREATE TABLE IF NOT EXISTS task_completions (
    id INTEGER PRIMARY KEY,
    completed_at TEXT NOT NULL,
    local_date TEXT NOT NULL,
    precision TEXT NOT NULL,
    source TEXT NOT NULL,
    path TEXT NOT NULL,
    task_text TEXT NOT NULL,
    done_date TEXT,
    outbox_id INTEGER,
    voided_at TEXT,
    detected_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS task_completions_outbox_id
    ON task_completions(outbox_id) WHERE outbox_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS task_completions_completed_at
    ON task_completions(completed_at);
CREATE TABLE IF NOT EXISTS task_snapshot (
    path TEXT NOT NULL,
    task_text TEXT NOT NULL,
    open_count INTEGER NOT NULL,
    closed_count INTEGER NOT NULL,
    PRIMARY KEY(path, task_text)
);
CREATE TABLE IF NOT EXISTS task_file_state (
    path TEXT PRIMARY KEY,
    fingerprint TEXT NOT NULL
);
";

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

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!(
        "SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"
    ))?;
    let count: i64 = stmt.query_row([column], |row| row.get(0))?;
    Ok(count > 0)
}

fn migrate_v2_to_v3(conn: &Connection) -> Result<()> {
    if !column_exists(conn, "outbox", "kind")? {
        conn.execute("ALTER TABLE outbox ADD COLUMN kind TEXT", [])?;
    }
    if !column_exists(conn, "outbox", "source_json")? {
        conn.execute("ALTER TABLE outbox ADD COLUMN source_json TEXT", [])?;
    }
    if !column_exists(conn, "outbox", "resolved_at")? {
        conn.execute("ALTER TABLE outbox ADD COLUMN resolved_at TEXT", [])?;
    }
    if !column_exists(conn, "outbox", "resolution")? {
        conn.execute("ALTER TABLE outbox ADD COLUMN resolution TEXT", [])?;
    }
    set_version(conn, 3)?;
    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn migrate_v3_to_v4(conn: &Connection) -> Result<()> {
    if table_exists(conn, "push_subscriptions")?
        && !column_exists(conn, "push_subscriptions", "last_sent_week")?
    {
        conn.execute(
            "ALTER TABLE push_subscriptions ADD COLUMN last_sent_week TEXT",
            [],
        )?;
    }
    // Version 4 is the weekly-spending stamp. Do not write SCHEMA_VERSION here.
    set_version(conn, 4)?;
    Ok(())
}

fn migrate_v4_to_v5(conn: &Connection) -> Result<()> {
    conn.execute_batch(V5_TABLES)?;
    set_version(conn, SCHEMA_VERSION)?;
    Ok(())
}

fn upgrade(conn: &Connection, version: i64) -> Result<()> {
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    if version == 1 {
        conn.execute_batch(V2_TABLES)?;
        seed_owner_account(conn)?;
        set_version(conn, 2)?;
        return upgrade(conn, 2);
    }
    if version == 2 {
        migrate_v2_to_v3(conn)?;
        return upgrade(conn, 3);
    }
    if version == 3 {
        migrate_v3_to_v4(conn)?;
        return upgrade(conn, 4);
    }
    if version == 4 {
        return migrate_v4_to_v5(conn);
    }
    Err(Error::Invalid(format!(
        "unsupported liberado.db schema version {version}"
    )))
}

/// Apply pending migrations. Idempotent when already at [`SCHEMA_VERSION`].
pub fn migrate(conn: &Connection) -> Result<()> {
    upgrade(conn, read_version(conn)?)
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

    #[test]
    fn bundled_sqlite_is_at_least_3_54() {
        let conn = Connection::open_in_memory().unwrap();
        let version: String = conn
            .query_row("SELECT sqlite_version()", [], |row| row.get(0))
            .unwrap();
        let mut parts = version.split('.');
        let major: u64 = parts.next().unwrap().parse().unwrap();
        let minor: u64 = parts.next().unwrap().parse().unwrap();
        assert!(
            major > 3 || (major == 3 && minor >= 54),
            "RUSTSEC-2026-0459 needs SQLite >= 3.54.0, got {version}"
        );
    }

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

    const V2_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS schema_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT OR IGNORE INTO schema_meta(key, value) VALUES ('version', '2');
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
CREATE TABLE IF NOT EXISTS accounts (
    id INTEGER PRIMARY KEY,
    handle TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    webauthn_user_id BLOB NOT NULL UNIQUE,
    created_at TEXT NOT NULL
);
INSERT OR IGNORE INTO accounts(id, handle, display_name, webauthn_user_id, created_at)
VALUES (1, 'owner', 'Owner', x'00', '2026-01-01T00:00:00Z');
";

    fn open_v2_with_rows(dir: &TempDir) -> Connection {
        let path = dir.path().join("liberado.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(V2_SCHEMA).unwrap();
        let now = stamp(Utc::now());
        conn.execute(
            "INSERT INTO outbox(idempotency_key, channel, target, title, body, state,
             next_attempt_at, created_at, due_at, ack_token_hash)
             VALUES ('k-v2', 'web_push', 'sub:1', 't', 'b', 'pending', ?1, ?1, ?1, x'00')",
            [now.as_str()],
        )
        .unwrap();
        conn
    }

    #[test]
    fn v2_database_migrates_to_v3_and_keeps_rows() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_v2_with_rows(&dir);
        migrate(&conn).unwrap();
        assert_eq!(read_version(&conn).unwrap(), SCHEMA_VERSION);
        let outbox_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get(0))
            .unwrap();
        assert_eq!(outbox_rows, 1);
        let kind_null: Option<String> = conn
            .query_row("SELECT kind FROM outbox WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert!(kind_null.is_none());
        assert!(column_exists(&conn, "outbox", "resolved_at").unwrap());
        assert!(column_exists(&conn, "outbox", "resolution").unwrap());
    }

    #[test]
    fn v1_database_migrates_through_v3_and_keeps_rows() {
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
        assert!(column_exists(&conn, "push_subscriptions", "last_sent_week").unwrap());
        assert_completion_tables(&conn);
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
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'task_completions'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
    }

    #[test]
    fn v3_database_gains_the_completion_log_and_keeps_outbox_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("liberado.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(V2_SCHEMA).unwrap();
        conn.execute_batch(
            "ALTER TABLE outbox ADD COLUMN kind TEXT;
             ALTER TABLE outbox ADD COLUMN source_json TEXT;
             ALTER TABLE outbox ADD COLUMN resolved_at TEXT;
             ALTER TABLE outbox ADD COLUMN resolution TEXT;
             UPDATE schema_meta SET value = '3' WHERE key = 'version';",
        )
        .unwrap();
        let now = stamp(Utc::now());
        conn.execute(
            "INSERT INTO outbox(idempotency_key, channel, target, title, body, state,
             next_attempt_at, created_at, due_at, ack_token_hash)
             VALUES ('k-v3', 'web_push', 'sub:1', 't', 'b', 'pending', ?1, ?1, ?1, x'00')",
            [now.as_str()],
        )
        .unwrap();
        migrate(&conn).unwrap();
        assert_eq!(read_version(&conn).unwrap(), SCHEMA_VERSION);
        let outbox_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
            .unwrap();
        assert_eq!(outbox_rows, 1);
        assert_completion_tables(&conn);
        migrate(&conn).unwrap();
        assert_eq!(read_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn v3_database_gains_the_week_stamp_and_keeps_the_phone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("liberado.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO schema_meta(key, value) VALUES ('version', '3');
             CREATE TABLE push_subscriptions (
                 id INTEGER PRIMARY KEY,
                 endpoint TEXT NOT NULL UNIQUE,
                 p256dh TEXT NOT NULL,
                 auth TEXT NOT NULL,
                 label TEXT NOT NULL,
                 created_at TEXT NOT NULL
             );
             INSERT INTO push_subscriptions(endpoint, p256dh, auth, label, created_at)
             VALUES ('https://push.example.test/phone', 'k', 'a', 'phone', '2026-10-01T00:00:00Z');",
        )
        .unwrap();
        migrate(&conn).unwrap();
        assert_eq!(read_version(&conn).unwrap(), SCHEMA_VERSION);
        let endpoint: String = conn
            .query_row(
                "SELECT endpoint FROM push_subscriptions WHERE last_sent_week IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(endpoint, "https://push.example.test/phone");
        assert_completion_tables(&conn);
    }

    #[test]
    fn fresh_database_reaches_v5_with_the_week_column_and_completion_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("liberado.db");
        drop(Outbox::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        assert_eq!(read_version(&conn).unwrap(), SCHEMA_VERSION);
        assert!(column_exists(&conn, "push_subscriptions", "last_sent_week").unwrap());
        assert_completion_tables(&conn);
    }

    #[test]
    fn v4_database_keeps_last_sent_week_and_gains_the_completion_tables() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open(dir.path().join("liberado.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO schema_meta(key, value) VALUES ('version', '4');
             CREATE TABLE push_subscriptions (
                 id INTEGER PRIMARY KEY,
                 endpoint TEXT NOT NULL UNIQUE,
                 p256dh TEXT NOT NULL,
                 auth TEXT NOT NULL,
                 label TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 last_sent_week TEXT
             );
             INSERT INTO push_subscriptions(
                 endpoint, p256dh, auth, label, created_at, last_sent_week
             ) VALUES (
                 'https://push.example.test/phone', 'k', 'a', 'phone',
                 '2026-10-01T00:00:00Z', '2026-11-02'
             );",
        )
        .unwrap();
        migrate(&conn).unwrap();
        assert_eq!(read_version(&conn).unwrap(), SCHEMA_VERSION);
        let week: String = conn
            .query_row(
                "SELECT last_sent_week FROM push_subscriptions WHERE endpoint = ?1",
                ["https://push.example.test/phone"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(week, "2026-11-02");
        assert_completion_tables(&conn);
        migrate(&conn).unwrap();
        assert_eq!(read_version(&conn).unwrap(), SCHEMA_VERSION);
        let week_again: String = conn
            .query_row(
                "SELECT last_sent_week FROM push_subscriptions WHERE endpoint = ?1",
                ["https://push.example.test/phone"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(week_again, "2026-11-02");
    }

    fn assert_completion_tables(conn: &Connection) {
        assert!(table_exists(conn, "task_completions").unwrap());
        assert!(column_exists(conn, "task_completions", "outbox_id").unwrap());
        assert!(table_exists(conn, "task_snapshot").unwrap());
        assert!(column_exists(conn, "task_snapshot", "open_count").unwrap());
        assert!(table_exists(conn, "task_file_state").unwrap());
        assert!(column_exists(conn, "task_file_state", "fingerprint").unwrap());
    }
}
