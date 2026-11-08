//! OpenCode (sst/opencode). Current versions keep everything in one SQLite database,
//! `<data>/opencode/opencode.db` (`~/.local/share/opencode` on every OS):
//!
//! * `session`  - id, directory, title, time_created/time_updated (ms), parent_id
//!   (set for subagent sessions), time_archived;
//! * `message`  - one row per turn, `data` JSON with `role` "user" | "assistant";
//! * `part`     - the pieces of a message, `data` JSON with `type` "text" | "tool" |
//!   "reasoning" | ...; `synthetic: true` marks text OpenCode injected itself.
//!
//! The database is opened read-only, so a running OpenCode (WAL mode) is not disturbed.

use crate::session::{Limits, Session};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

pub fn load(root: &Path, lim: &Limits, limit: usize) -> Vec<Session> {
    let db = root.join("opencode.db");
    if !db.is_file() {
        return Vec::new();
    }
    read(&db, lim, limit).unwrap_or_default()
}

fn columns(conn: &Connection, table: &str) -> rusqlite::Result<HashSet<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |r| r.get::<_, String>(1))?.collect::<Result<_, _>>()?;
    Ok(names)
}

fn read(db: &Path, lim: &Limits, limit: usize) -> rusqlite::Result<Vec<Session>> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.busy_timeout(Duration::from_millis(800))?;
    // Columns came and went across versions; only ask for what this file has.
    let cols = columns(&conn, "session")?;
    let parent = if cols.contains("parent_id") { "parent_id" } else { "NULL" };
    let archived = if cols.contains("time_archived") { "WHERE time_archived IS NULL" } else { "" };
    let sql = format!(
        "SELECT id, directory, title, time_created, time_updated, {parent} FROM session {archived} \
         ORDER BY time_updated DESC LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(String, String, String, i64, i64, Option<String>)> = stmt
        .query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
        .collect::<Result<_, _>>()?;

    let mut parts = conn.prepare(
        "SELECT m.data, p.data FROM message m JOIN part p ON p.message_id = m.id \
         WHERE m.session_id = ?1 ORDER BY m.time_created, m.id, p.id",
    )?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, dir, title, created, updated, parent) in rows {
        let mut s = Session {
            id: id.clone(),
            cwd: dir,
            started_ms: created,
            updated_ms: updated,
            file: db.to_path_buf(),
            entry: parent.map(|_| "subagent".to_string()),
            custom_title: real_title(&title),
            ..Default::default()
        };
        let mut current_msg = String::new();
        let mut text = String::new();
        let pieces = parts.query_map([&id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for piece in pieces.flatten() {
            let (msg, part) = piece;
            // Parts of one message are joined into one prompt.
            if msg != current_msg {
                s.push_prompt(&std::mem::take(&mut text), lim);
                current_msg = msg.clone();
            }
            if let Some(t) = user_text(&msg, &part) {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&t);
            }
        }
        s.push_prompt(&text, lim);
        out.push(s);
    }
    Ok(out)
}

/// The text of a part, if it belongs to a user message and was typed by the user.
fn user_text(message: &str, part: &str) -> Option<String> {
    let m: Value = serde_json::from_str(message).ok()?;
    if m.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let p: Value = serde_json::from_str(part).ok()?;
    if p.get("type").and_then(Value::as_str) != Some("text")
        || p.get("synthetic").and_then(Value::as_bool) == Some(true)
        || p.get("ignored").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    p.get("text").and_then(Value::as_str).map(str::to_string)
}

/// OpenCode names untitled sessions "New session - <ISO date>" / "Child session - ...";
/// those are no title at all, the first prompt says more.
fn real_title(t: &str) -> Option<String> {
    let t = t.trim();
    if t.is_empty() || t.starts_with("New session - ") || t.starts_with("Child session - ") {
        None
    } else {
        Some(t.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIM: Limits = Limits { max_prompt_chars: 4000, max_indexed_chars: 40000 };

    #[test]
    fn reads_a_database() {
        let dir = std::env::temp_dir().join(format!("chs-oc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("opencode.db");
        let _ = std::fs::remove_file(&db);
        let c = Connection::open(&db).unwrap();
        c.execute_batch(
            r#"
            CREATE TABLE session (id text PRIMARY KEY, project_id text, parent_id text, slug text, directory text,
                title text, version text, time_created integer, time_updated integer, time_archived integer);
            CREATE TABLE message (id text PRIMARY KEY, session_id text, time_created integer, time_updated integer, data text);
            CREATE TABLE part (id text PRIMARY KEY, message_id text, session_id text, time_created integer,
                time_updated integer, data text);
            INSERT INTO session VALUES ('ses_b','global',NULL,'a','D:/work/app','New session - 2026-06-21T12:26:43.066Z','1.17.11',100,300,NULL);
            INSERT INTO session VALUES ('ses_old','global',NULL,'b','/w','Old','1.17.11',10,20,NULL);
            INSERT INTO session VALUES ('ses_arch','global',NULL,'c','/w','Archived','1.17.11',10,400,500);
            INSERT INTO session VALUES ('ses_kid','global','ses_b','d','/w','Child session - x','1.17.11',150,250,NULL);
            INSERT INTO message VALUES ('msg_1','ses_b',110,110,'{"role":"user","agent":"build","time":{"created":110}}');
            INSERT INTO part VALUES ('prt_1','msg_1','ses_b',110,110,'{"type":"text","text":"fix the login bug"}');
            INSERT INTO part VALUES ('prt_2','msg_1','ses_b',110,110,'{"type":"text","text":"injected","synthetic":true}');
            INSERT INTO message VALUES ('msg_2','ses_b',120,120,'{"role":"assistant","mode":"build"}');
            INSERT INTO part VALUES ('prt_3','msg_2','ses_b',120,120,'{"type":"text","text":"done"}');
            INSERT INTO message VALUES ('msg_3','ses_b',130,130,'{"role":"user"}');
            INSERT INTO part VALUES ('prt_4','msg_3','ses_b',130,130,'{"type":"text","text":"now add tests"}');
            "#,
        )
        .unwrap();
        drop(c);
        let v = load(&dir, &LIM, 50);
        let ids: Vec<&str> = v.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["ses_b", "ses_kid", "ses_old"], "newest first, archived hidden");
        assert_eq!(v[0].prompts, vec!["fix the login bug", "now add tests"]);
        assert_eq!(v[0].title(), "fix the login bug", "default title is ignored");
        assert_eq!(v[0].cwd, "D:/work/app");
        assert_eq!(v[1].entry.as_deref(), Some("subagent"));
        assert_eq!(v[2].title(), "Old");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
