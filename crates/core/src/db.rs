//! SQLite storage: one connection, serialized behind a mutex.
//!
//! StoryForge is a single-user desktop application, so one writer is the
//! correct concurrency model. Callers that need a transaction use
//! [`Db::with_tx`], which commits on `Ok` and rolls back on `Err`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, Transaction, params};

use crate::error::{Result, SfError};

/// The schema shipped with the application. Executing it is idempotent:
/// every statement uses `CREATE ... IF NOT EXISTS`.
const SCHEMA: &str = include_str!("../assets/schema.sql");

pub struct Db {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl Db {
    /// Open (or create) the database at `path`, applying pragmas and schema.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| SfError::io(parent, e))?;
        }
        let conn = Connection::open(&path).map_err(|e| SfError::Sql(e))?;
        configure(&conn)?;
        conn.execute_batch(SCHEMA)?;
        tracing::info!(path = %path.display(), "database ready");
        Ok(Self {
            conn: Mutex::new(conn),
            path,
        })
    }

    /// Open the database resolved by [`crate::paths::resolve_db_path`].
    pub fn open_default() -> Result<Self> {
        Self::open(crate::paths::resolve_db_path())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Run `f` with exclusive access to the connection.
    ///
    /// The closure chooses its own error type; `SfError` converts into it, so a
    /// handler can surface `ApiError` directly without wrapping.
    pub fn with_conn<T, E>(&self, f: impl FnOnce(&Connection) -> std::result::Result<T, E>) -> std::result::Result<T, E>
    where
        E: From<SfError>,
    {
        let guard = self.conn.lock().map_err(|_| {
            E::from(SfError::Other("database mutex poisoned".into()))
        })?;
        f(&guard)
    }

    /// Run `f` inside a transaction. Commits when `f` returns `Ok`.
    pub fn with_tx<T, E>(&self, f: impl FnOnce(&Transaction<'_>) -> std::result::Result<T, E>) -> std::result::Result<T, E>
    where
        E: From<SfError>,
    {
        let mut guard = self.conn.lock().map_err(|_| {
            E::from(SfError::Other("database mutex poisoned".into()))
        })?;
        let tx = guard.transaction().map_err(|e| E::from(SfError::Sql(e)))?;
        let value = f(&tx)?;
        tx.commit().map_err(|e| E::from(SfError::Sql(e)))?;
        Ok(value)
    }

    /// `PRAGMA quick_check` â€” cheap integrity probe used by the About screen.
    pub fn quick_check(&self) -> Result<String> {
        self.with_conn(|conn| {
            let result: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
            Ok(result)
        })
    }

    /// Highest migration applied to this database (from `PRAGMA user_version`).
    pub fn user_version(&self) -> Result<i64> {
        self.with_conn(|conn| {
            let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            Ok(version)
        })
    }

    /// Number of books in the database â€” used by the first-run screen.
    pub fn book_count(&self) -> Result<i64> {
        self.with_conn(|conn| {
            let count: i64 = conn.query_row("SELECT COUNT(*) FROM book", [], |row| row.get(0))?;
            Ok(count)
        })
    }

    /// Attach the database file used by an existing StoryForge installation.
    /// The file is copied next to the configured database, never moved.
    pub fn import_from(source: impl AsRef<Path>) -> Result<PathBuf> {
        let source = source.as_ref();
        if !source.is_file() {
            return Err(SfError::NotFound(format!(
                "database not found: {}",
                source.display()
            )));
        }
        let target = crate::paths::resolve_db_path();
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| SfError::io(parent, e))?;
        }
        let backup = target.with_extension("db.before-import");
        if target.is_file() {
            std::fs::rename(&target, &backup).map_err(|e| SfError::io(&target, e))?;
        }
        std::fs::copy(source, &target).map_err(|e| SfError::io(&target, e))?;
        Ok(target)
    }
}

fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

/// Convenience for statements that only need the row count.
pub fn changes(conn: &Connection, sql: &str, args: impl rusqlite::Params) -> Result<usize> {
    Ok(conn.execute(sql, args)?)
}

/// `SELECT 1` connectivity probe.
pub fn ping(conn: &Connection) -> Result<()> {
    let one: i64 = conn.query_row("SELECT 1", [], |row| row.get(0))?;
    debug_assert_eq!(one, 1);
    let _ = params![];
    Ok(())
}
