//! A thin wrapper over SQLite — one connection, used from one thread.
//!
//! This reproduces `Sources/DigUpKit/Database.swift` verbatim (ARCHITECTURE_MAP.md §2), so a
//! rusqlite-opened index behaves byte-for-byte like the Swift engine's: WAL, `foreign_keys=ON`,
//! the 1 GiB mmap, the WAL-switch retry, and nested `BEGIN IMMEDIATE`/savepoint transactions.

use std::cell::Cell;
use std::path::Path;
use std::time::Duration;

use rusqlite::types::Value;
use rusqlite::{ffi, Connection, Row, ToSql};
use thiserror::Error;

pub const BUSY_TIMEOUT_MS: u64 = 5000;
/// Migration widens the wait: the app and its helper can open the index at once; one migrates
/// while the other waits for it (Swift: `PRAGMA busy_timeout = 60000`, Database.swift:211).
pub const MIGRATION_BUSY_TIMEOUT_MS: u64 = 60000;
/// Keystroke keyword search spends most of its time pinning purgeable page-cache pages; mapping
/// the file skips that path (issue #93; Database.swift:41).
pub const MMAP_SIZE: i64 = 1_073_741_824;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    /// The engine's own refusal (`Database.Failure`): a message, not a SQLite code.
    #[error("{0}")]
    Failure(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn ok<T>(v: T) -> Result<T> {
    Ok(v)
}

/// Bind value, mirroring `SQLValue` (Database.swift:111): int / double / text / blob / null.
pub enum Sql {
    Int(i64),
    Double(f64),
    Text(String),
    Blob(Vec<u8>),
    Null,
}

impl Sql {
    pub fn opt_text(s: Option<&str>) -> Sql {
        s.map(|t| Sql::Text(t.to_string())).unwrap_or(Sql::Null)
    }
    pub fn opt_double(d: Option<f64>) -> Sql {
        d.map(Sql::Double).unwrap_or(Sql::Null)
    }
    fn into_value(self) -> Value {
        match self {
            Sql::Int(i) => Value::Integer(i),
            Sql::Double(d) => Value::Real(d),
            Sql::Text(t) => Value::Text(t),
            Sql::Blob(b) => Value::Blob(b),
            Sql::Null => Value::Null,
        }
    }
}

/// Row accessors mirroring `Statement`'s (Database.swift:167): int / double / text / blob / isNull.
pub struct RowView<'a, 'b> {
    row: &'a Row<'b>,
}

impl<'a, 'b> RowView<'a, 'b> {
    pub fn int(&self, col: usize) -> i64 {
        self.row.get::<_, i64>(col).unwrap_or(0)
    }
    pub fn double(&self, col: usize) -> f64 {
        self.row.get::<_, f64>(col).unwrap_or(0.0)
    }
    pub fn text(&self, col: usize) -> Option<String> {
        self.row.get::<_, String>(col).ok().or_else(|| {
            self.row
                .get::<_, Vec<u8>>(col)
                .ok()
                .map(|b| String::from_utf8_lossy(&b).into_owned())
        })
    }
    pub fn is_null(&self, col: usize) -> bool {
        matches!(
            self.row.get_ref(col).ok().map(|v| v.data_type()),
            Some(rusqlite::types::Type::Null)
        )
    }
}

pub struct Database {
    conn: Connection,
    depth: Cell<usize>,
}

impl Database {
    /// Opens readwrite|create, sets `busy_timeout`, switches to WAL (retrying the WAL switch for
    /// up to five seconds — two processes opening a new index can dead-lock the WAL switch,
    /// Database.swift:29), then enables the mmap.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))?;
        let mut tries = 0;
        loop {
            // journal_mode=WAL returns a row; execute_batch tolerates the returned row.
            match conn.execute_batch(
                "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;",
            ) {
                Ok(()) => break,
                Err(e) if is_busy(&e) && tries < 50 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(e.into()),
            }
        }
        conn.execute_batch(&format!("PRAGMA mmap_size = {MMAP_SIZE}"))?;
        Ok(Self {
            conn,
            depth: Cell::new(0),
        })
    }

    pub fn set_busy_timeout_ms(&self, ms: u64) -> Result<()> {
        self.conn.busy_timeout(Duration::from_millis(ms))?;
        Ok(())
    }

    pub fn user_version(&self) -> Result<i64> {
        self.conn
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .map_err(Into::into)
    }

    pub fn set_user_version(&self, v: i64) -> Result<()> {
        self.conn
            .execute_batch(&format!("PRAGMA user_version = {v}"))
            .map_err(Into::into)
    }

    /// `execute` (Database.swift:46): run one or more statements, no bound values.
    pub fn execute(&self, sql: &str) -> Result<()> {
        self.conn.execute_batch(sql).map_err(Into::into)
    }

    /// `query` (Database.swift:56): bind `values`, call `row` for each result row.
    pub fn query(
        &self,
        sql: &str,
        values: impl IntoIterator<Item = Sql>,
        mut row: impl FnMut(&RowView) -> Result<()>,
    ) -> Result<()> {
        let owned: Vec<Value> = values.into_iter().map(Sql::into_value).collect();
        let params: Vec<&dyn ToSql> = owned.iter().map(|v| v as &dyn ToSql).collect();
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query(params.as_slice())?;
        while let Some(r) = rows.next()? {
            let view = RowView { row: r };
            row(&view)?;
        }
        Ok(())
    }

    pub fn run(&self, sql: &str, values: impl IntoIterator<Item = Sql>) -> Result<()> {
        self.query(sql, values, |_| Ok(()))
    }

    /// `run(keeping:)` (Database.swift:70): prepare once and reuse per row. The counters
    /// migration updates 1.2M rows one at a time; preparing per row costs more than the update.
    pub fn run_keeping(
        &self,
        sql: &str,
        rows: impl FnOnce(&mut dyn FnMut(Vec<Sql>) -> Result<()>) -> Result<()>,
    ) -> Result<()> {
        let mut stmt = self.conn.prepare(sql)?;
        let mut bind = |values: Vec<Sql>| -> Result<()> {
            let owned: Vec<Value> = values.into_iter().map(Sql::into_value).collect();
            let params: Vec<&dyn ToSql> = owned.iter().map(|v| v as &dyn ToSql).collect();
            stmt.execute(params.as_slice())?;
            Ok(())
        };
        rows(&mut bind)?;
        Ok(())
    }

    /// Nested transaction (Database.swift:86): depth 0 is `BEGIN IMMEDIATE`, deeper is a savepoint
    /// whose failure undoes only its own writes and lets the outer one continue.
    pub fn transaction<T>(&self, body: impl FnOnce() -> Result<T>) -> Result<T> {
        let depth = self.depth.get();
        self.depth.set(depth + 1);
        let savepoint = format!("digup{depth}");
        let begin = if depth == 0 {
            "BEGIN IMMEDIATE".to_string()
        } else {
            format!("SAVEPOINT {savepoint}")
        };
        self.execute(&begin)?;
        match body() {
            Ok(v) => {
                let commit = if depth == 0 {
                    "COMMIT".to_string()
                } else {
                    format!("RELEASE {savepoint}")
                };
                self.execute(&commit)?;
                self.depth.set(depth);
                Ok(v)
            }
            Err(e) => {
                if depth == 0 {
                    let _ = self.execute("ROLLBACK");
                } else {
                    let _ = self.execute(&format!("ROLLBACK TO {savepoint}"));
                    let _ = self.execute(&format!("RELEASE {savepoint}"));
                }
                self.depth.set(depth);
                Err(e)
            }
        }
    }

    pub fn last_insert_rowid(&self) -> i64 {
        self.conn.last_insert_rowid()
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

fn is_busy(e: &rusqlite::Error) -> bool {
    // SQLITE_BUSY maps to ErrorCode::DatabaseBusy (libsqlite3-sys's ErrorCode enum).
    matches!(e.sqlite_error_code(), Some(ffi::ErrorCode::DatabaseBusy))
}
