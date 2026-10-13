//! The SQLite store: connection handling and versioned migrations.

use nash_core::error::{EngineError, Result};
use parking_lot::Mutex;
use rusqlite::Connection;
use std::path::Path;

/// Current on-disk schema version. Bump and add a migration arm when the
/// schema changes.
pub const SCHEMA_VERSION: i64 = 1;

/// Handle to the sessions/settings database.
///
/// `rusqlite::Connection` is `Send` but not `Sync`; a mutex makes the store
/// shareable across threads. All access is synchronous, which is correct for
/// a local SQLite file and keeps the async surface simple.
pub struct Store {
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    /// Open (creating if needed) a database file and run migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path).map_err(db_err)?;
        Self::from_conn(conn)
    }

    /// An ephemeral database, for tests and previews.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(db_err)?;
        Self::from_conn(conn)
    }

    fn from_conn(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON").map_err(db_err)?;
        // WAL is best-effort: some filesystems (and :memory:) reject it.
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    /// Apply the migrations required to reach [`SCHEMA_VERSION`].
    pub fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock();
        let current: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(db_err)?;
        if current >= SCHEMA_VERSION {
            return Ok(());
        }
        if current < 1 {
            conn.execute_batch(SCHEMA_V1).map_err(db_err)?;
        }
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(db_err)?;
        tracing::info!(from = current, to = SCHEMA_VERSION, "migrated store");
        Ok(())
    }

    /// Run a closure with a shared connection reference.
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.conn.lock();
        f(&conn)
    }

    /// Run a closure inside a transaction, committing on success.
    pub fn with_tx<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(db_err)?;
        let out = f(&tx)?;
        tx.commit().map_err(db_err)?;
        Ok(out)
    }
}

fn db_err(e: rusqlite::Error) -> EngineError {
    EngineError::Other(format!("database error: {e}"))
}

const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id          TEXT PRIMARY KEY,
    title       TEXT,
    provider    TEXT,
    model       TEXT,
    pinned      INTEGER NOT NULL DEFAULT 0,
    tags        TEXT NOT NULL DEFAULT '[]',
    metadata    TEXT NOT NULL DEFAULT '{}',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
    id          TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL,
    role        TEXT NOT NULL,
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);

CREATE TABLE IF NOT EXISTS usage (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id          TEXT,
    provider            TEXT NOT NULL,
    model               TEXT NOT NULL,
    prompt_tokens       INTEGER NOT NULL DEFAULT 0,
    completion_tokens   INTEGER NOT NULL DEFAULT 0,
    total_tokens        INTEGER NOT NULL DEFAULT 0,
    latency_ms          INTEGER NOT NULL DEFAULT 0,
    streaming           INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_usage_provider ON usage(provider, created_at);
"#;
