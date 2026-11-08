//! OpenCode (anomalyco/opencode, formerly sst/opencode). One SQLite database,
//! `<data>/opencode/opencode.db` (`~/.local/share/opencode` on every OS), which since
//! 2.x holds two generations of tables side by side:
//!
//! * V1 (1.2 - 1.19): `session` + `message` (`data.role` user|assistant) + `part`
//!   (`data.type` "text", `synthetic`/`ignored` flags mark injected text);
//! * V2 (2.x): `session_v2` + `session_message`, where a prompt is simply a row with
//!   `type = 'user'` and the text in `data.text`.
//!
//! V2 copies the V1 sessions once with the same ids and leaves the old tables, and a
//! user may keep running a 1.x build afterwards - so both are read and, per id, the
//! more recently updated row wins. `parent_id` marks a subagent session. Before 1.2
//! sessions were JSON files under `storage/`; those builds are long gone and the
//! files are a dead leftover, so they are not read.

use super::sqlite;
use crate::session::{Limits, Session};
use rusqlite::Connection;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

pub fn load(root: &Path, lim: &Limits, limit: usize) -> Vec<Session> {
    let db = root.join("opencode.db");
    if !db.is_file() {
        return Vec::new();
    }
    read(&db, lim, limit).unwrap_or_default()
}

fn read(db: &Path, lim: &Limits, limit: usize) -> rusqlite::Result<Vec<Session>> {
    let conn = sqlite::open(db)?;
    let tables = sqlite::tables(&conn)?;
    let mut by_id: HashMap<String, Session> = HashMap::new();
    let mut keep = |s: Session| {
        if by_id.get(&s.id).is_none_or(|old| old.updated_ms < s.updated_ms) {
            by_id.insert(s.id.clone(), s);
        }
    };
    if tables.contains("session_v2") && tables.contains("session_message") {
        for s in sessions(&conn, "session_v2", db, limit)? {
            keep(with_v2_prompts(&conn, s, lim)?);
        }
    }
    if tables.contains("session") && tables.contains("message") && tables.contains("part") {
        for s in sessions(&conn, "session", db, limit)? {
            keep(with_v1_prompts(&conn, s, lim)?);
        }
    }
    let mut out: Vec<Session> = by_id.into_values().collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
    out.truncate(limit);
    Ok(out)
}

fn sessions(conn: &Connection, table: &str, db: &Path, limit: usize) -> rusqlite::Result<Vec<Session>> {
    let cols = sqlite::columns(conn, table)?;
    let parent = sqlite::col_or(&cols, "parent_id", "NULL");
    let archived = if cols.contains("time_archived") { "WHERE time_archived IS NULL" } else { "" };
    let sql = format!(
        "SELECT id, directory, title, time_created, time_updated, {parent} FROM {table} {archived} \
         ORDER BY time_updated DESC LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([limit as i64], |r| {
        let title: Option<String> = r.get(2)?;
        let parent: Option<String> = r.get(5)?;
        Ok(Session {
            id: r.get(0)?,
            cwd: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            custom_title: title.as_deref().and_then(real_title),
            started_ms: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
            updated_ms: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
            entry: parent.map(|_| "subagent".to_string()),
            file: db.to_path_buf(),
            ..Default::default()
        })
    })?;
    rows.collect()
}

fn with_v2_prompts(conn: &Connection, mut s: Session, lim: &Limits) -> rusqlite::Result<Session> {
    let mut stmt = conn.prepare_cached(
        "SELECT data FROM session_message WHERE session_id = ?1 AND type = 'user' ORDER BY seq, time_created",
    )?;
    for data in stmt.query_map([&s.id], |r| r.get::<_, String>(0))?.flatten() {
        if let Some(t) = serde_json::from_str::<Value>(&data).ok().and_then(|v| v["text"].as_str().map(String::from)) {
            s.push_prompt(&t, lim);
        }
    }
    Ok(s)
}

fn with_v1_prompts(conn: &Connection, mut s: Session, lim: &Limits) -> rusqlite::Result<Session> {
    let mut stmt = conn.prepare_cached(
        "SELECT m.id, m.data, p.data FROM message m JOIN part p ON p.message_id = m.id \
         WHERE m.session_id = ?1 ORDER BY m.time_created, m.id, p.id",
    )?;
    let mut current = String::new();
    let mut text = String::new();
    let rows =
        stmt.query_map([&s.id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
    for (mid, msg, part) in rows.flatten() {
        // The text parts of one message make one prompt.
        if mid != current {
            s.push_prompt(&std::mem::take(&mut text), lim);
            current = mid;
        }
        if let Some(t) = v1_user_text(&msg, &part) {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&t);
        }
    }
    s.push_prompt(&text, lim);
    Ok(s)
}

/// The text of a V1 part, if it belongs to a user message and was typed by the user.
fn v1_user_text(message: &str, part: &str) -> Option<String> {
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

/// Untitled sessions are named "New session - <ISO date>" / "Child session - ...";
/// that is no title at all, the first prompt says more.
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

    fn temp_db(name: &str, sql: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("chs-oc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Connection::open(dir.join("opencode.db")).unwrap().execute_batch(sql).unwrap();
        dir
    }

    const V1: &str = r#"
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
    "#;

    #[test]
    fn reads_v1() {
        let dir = temp_db("v1", V1);
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

    #[test]
    fn reads_v2_and_merges_with_v1() {
        let sql = format!(
            "{V1}{}",
            r#"
            CREATE TABLE session_v2 (id text PRIMARY KEY, project_id text, parent_id text, directory text, path text,
                title text, time_created integer, time_updated integer, time_archived integer);
            CREATE TABLE session_message (id text PRIMARY KEY, session_id text, type text, seq integer,
                time_created integer, data text);
            -- ses_b was copied from V1 and then continued in 2.x: V2 row is newer.
            INSERT INTO session_v2 VALUES ('ses_b','global',NULL,'D:/work/app','', NULL, 100, 900, NULL);
            INSERT INTO session_message VALUES ('msg_a','ses_b','user',1,110,'{"text":"fix the login bug"}');
            INSERT INTO session_message VALUES ('msg_b','ses_b','assistant',2,120,'{"text":"done"}');
            INSERT INTO session_message VALUES ('msg_c','ses_b','synthetic',3,121,'{"text":"reminder"}');
            INSERT INTO session_message VALUES ('msg_d','ses_b','user',4,800,'{"text":"ship it"}');
            -- ses_old was copied to V2, then continued in 1.x: the V1 row (20) beats the copy (5).
            INSERT INTO session_v2 VALUES ('ses_old','global',NULL,'/w','', 'Old', 10, 5, NULL);
            INSERT INTO session_v2 VALUES ('ses_new','global',NULL,'/x','', 'Only in V2', 1000, 1000, NULL);
            "#
        );
        let dir = temp_db("v2", &sql);
        let v = load(&dir, &LIM, 50);
        let ids: Vec<&str> = v.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["ses_new", "ses_b", "ses_kid", "ses_old"]);
        let b = &v[1];
        assert_eq!(b.prompts, vec!["fix the login bug", "ship it"], "V2 row wins, only type=user");
        assert_eq!(b.updated_ms, 900);
        assert_eq!(v[3].updated_ms, 20, "newer V1 row wins for ses_old");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
