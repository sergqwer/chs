//! Shared bits for database-backed sources: every tool here keeps its SQLite file in
//! WAL mode and may be running while chs reads, so connections are read-only with a
//! short busy timeout, and schemas are probed instead of assumed (columns come and go
//! between versions).

use rusqlite::{Connection, OpenFlags};
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

/// Run `f` on the database file `db` if it exists; a database that can not be read
/// (locked past the timeout, an unknown schema) yields nothing.
pub fn with_db<T>(db: &Path, f: impl FnOnce(&Connection) -> rusqlite::Result<Vec<T>>) -> Vec<T> {
    if !db.is_file() {
        return Vec::new();
    }
    open(db).and_then(|c| f(&c)).unwrap_or_default()
}

pub fn open(db: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.busy_timeout(Duration::from_millis(800))?;
    Ok(conn)
}

pub fn tables(conn: &Connection) -> rusqlite::Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
    let names = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?;
    Ok(names)
}

pub fn columns(conn: &Connection, table: &str) -> rusqlite::Result<HashSet<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let names = stmt.query_map([], |r| r.get::<_, String>(1))?.collect::<Result<_, _>>()?;
    Ok(names)
}

/// `name` if the column exists, else a SQL literal to select in its place.
pub fn col_or<'a>(cols: &HashSet<String>, name: &'a str, fallback: &'a str) -> &'a str {
    if cols.contains(name) {
        name
    } else {
        fallback
    }
}
