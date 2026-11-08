//! Goose (block/goose, now aaif-goose/goose), 1.10 and later: one SQLite database,
//! `<data>/sessions/sessions.db` - `~/.local/share/goose` on Linux and macOS,
//! `%APPDATA%\Block\goose\data` on Windows.
//!
//! * `sessions` - id ("20261108_3"), working_dir, name (default "CLI Session"; older
//!   rows used `description`), session_type (user | scheduled | sub_agent | hidden |
//!   terminal | gateway | acp), parent_session_id, archived_at, created_at/updated_at
//!   as TEXT timestamps (usually "YYYY-MM-DD HH:MM:SS" UTC, sometimes RFC 3339);
//! * `messages` - role user|assistant, `content_json` = array of typed parts. Tool
//!   results are stored with role "user" too, so only `{"type":"text"}` parts of
//!   messages the user could see (`metadata_json.userVisible` != false) count.

use super::{sqlite, Reuse};
use crate::session::{Limits, Session};
use rusqlite::Connection;
use serde_json::Value;
use std::path::Path;

pub fn load(root: &Path, lim: &Limits, limit: usize, reuse: &Reuse) -> Vec<Session> {
    let db = root.join("sessions").join("sessions.db");
    sqlite::with_db(&db, |conn| read(conn, &db, lim, limit, reuse))
}

fn read(conn: &Connection, db: &Path, lim: &Limits, limit: usize, reuse: &Reuse) -> rusqlite::Result<Vec<Session>> {
    let cols = sqlite::columns(conn, "sessions")?;
    let name = sqlite::col_or(&cols, "name", "''");
    let description = sqlite::col_or(&cols, "description", "''");
    let kind = sqlite::col_or(&cols, "session_type", "'user'");
    let archived = if cols.contains("archived_at") { "WHERE archived_at IS NULL" } else { "" };
    // unixepoch() copes with both timestamp spellings Goose has written.
    let sql = format!(
        "SELECT id, working_dir, {name}, {description}, {kind}, \
                unixepoch(created_at) * 1000, unixepoch(updated_at) * 1000 \
         FROM sessions {archived} ORDER BY unixepoch(updated_at) DESC LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let sessions: Vec<Session> = stmt
        .query_map([limit as i64], |r| {
            let name: Option<String> = r.get(2)?;
            let description: Option<String> = r.get(3)?;
            let title = [name, description].into_iter().flatten().find(|t| !t.trim().is_empty() && t != "CLI Session");
            Ok(Session {
                id: r.get(0)?,
                cwd: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                custom_title: title,
                entry: r.get(4)?,
                started_ms: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                updated_ms: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                file: db.to_path_buf(),
                ..Default::default()
            })
        })?
        .collect::<Result<_, _>>()?;
    sessions
        .into_iter()
        .map(|mut s| {
            if !reuse.fill(&mut s) {
                prompts(conn, &mut s, lim)?;
            }
            Ok(s)
        })
        .collect()
}

fn prompts(conn: &Connection, s: &mut Session, lim: &Limits) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare_cached(
        "SELECT content_json, metadata_json FROM messages WHERE session_id = ?1 AND role = 'user' ORDER BY id",
    )?;
    let rows = stmt.query_map([&s.id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?;
    for (content, meta) in rows.flatten() {
        if let Some(t) = user_text(&content, meta.as_deref()) {
            s.push_prompt(&t, lim);
        }
    }
    Ok(())
}

fn user_text(content: &str, meta: Option<&str>) -> Option<String> {
    if let Some(m) = meta.and_then(|m| serde_json::from_str::<Value>(m).ok()) {
        if m.get("userVisible").and_then(Value::as_bool) == Some(false) {
            return None;
        }
    }
    let parts: Vec<Value> = serde_json::from_str(content).ok()?;
    let texts: Vec<&str> = parts
        .iter()
        .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::common::testing::*;

    #[test]
    fn reads_sessions() {
        let root = TempDir::new("goose");
        std::fs::create_dir_all(root.join("sessions")).unwrap();
        Connection::open(root.join("sessions/sessions.db"))
            .unwrap()
            .execute_batch(
                r#"
                CREATE TABLE sessions (id TEXT PRIMARY KEY, name TEXT NOT NULL DEFAULT '', description TEXT NOT NULL DEFAULT '',
                    session_type TEXT NOT NULL DEFAULT 'user', working_dir TEXT NOT NULL,
                    created_at TIMESTAMP, updated_at TIMESTAMP, archived_at TIMESTAMP, parent_session_id TEXT);
                CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, message_id TEXT, session_id TEXT, role TEXT,
                    content_json TEXT, created_timestamp INTEGER, metadata_json TEXT);
                INSERT INTO sessions VALUES ('20261108_1','CLI Session','','user','/home/u/app','2026-11-08 10:00:00','2026-11-08 11:00:00',NULL,NULL);
                INSERT INTO sessions VALUES ('20261108_2','Deploy fix','','sub_agent','/home/u/app','2026-11-08 10:30:00','2026-11-08T12:00:00Z',NULL,'20261108_1');
                INSERT INTO sessions VALUES ('20261107_1','Old','','user','/x','2026-11-07 10:00:00','2026-11-07 10:00:00','2026-11-07 11:00:00',NULL);
                INSERT INTO messages (session_id, role, content_json, metadata_json) VALUES
                  ('20261108_1','user','[{"type":"text","text":"why is the build red"}]','{"userVisible":true}'),
                  ('20261108_1','assistant','[{"type":"text","text":"checking"}]',NULL),
                  ('20261108_1','user','[{"type":"toolResponse","id":"t1"}]',NULL),
                  ('20261108_1','user','[{"type":"text","text":"hidden context"}]','{"userVisible":false}'),
                  ('20261108_1','user','[{"type":"text","text":"fix it"}]',NULL);
                "#,
            )
            .unwrap();
        let lim = Limits::TEST;
        let v = load(&root, &lim, 10, &fresh(crate::config::Format::Goose));
        let ids: Vec<&str> = v.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["20261108_2", "20261108_1"], "RFC 3339 and plain timestamps both sort; archived hidden");
        assert_eq!(v[1].prompts, vec!["why is the build red", "fix it"]);
        assert_eq!(v[1].title(), "why is the build red", "\"CLI Session\" is no title");
        assert_eq!(v[1].entry.as_deref(), Some("user"));
        assert_eq!(v[0].entry.as_deref(), Some("sub_agent"));
        let expect = chrono::DateTime::parse_from_rfc3339("2026-11-08T11:00:00Z").unwrap().timestamp_millis();
        assert_eq!(v[1].updated_ms, expect, "naive timestamps are UTC");
    }
}
