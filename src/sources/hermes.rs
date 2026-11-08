//! Hermes Agent (NousResearch): one SQLite database, `~/.hermes/state.db`
//! (`$HERMES_HOME`, `%LOCALAPPDATA%\hermes` on Windows), rows updated in place.
//!
//! * `sessions` - id, source (cli, tui, oneshot, cron, subagent, acp, a messaging
//!   gateway...), cwd, title, started_at / last_activity_at in seconds (REAL),
//!   parent_session_id, archived, hidden;
//! * `messages` - session_id, role, content, active (0 once rewound or compacted
//!   away), display_kind / display_metadata (hidden and model-only rows). Compaction
//!   summaries and system notes are stored as user rows; their fixed prefixes mark them.

use super::common::text_of;
use super::sqlite;
use crate::session::{Limits, Session};
use rusqlite::Connection;
use serde_json::Value;
use std::path::Path;

pub fn load(root: &Path, lim: &Limits, limit: usize) -> Vec<Session> {
    let db = root.join("state.db");
    if !db.is_file() {
        return Vec::new();
    }
    read(&db, lim, limit).unwrap_or_default()
}

fn read(db: &Path, lim: &Limits, limit: usize) -> rusqlite::Result<Vec<Session>> {
    let conn = sqlite::open(db)?;
    let cols = sqlite::columns(&conn, "sessions")?;
    let has = |c: &str| cols.contains(c);
    let col = |c: &'static str, or: &'static str| sqlite::col_or(&cols, c, or);
    let updated: Vec<&str> =
        ["last_activity_at", "ended_at", "started_at"].into_iter().filter(|c| has(c)).collect::<Vec<_>>();
    let updated = if updated.is_empty() { "0".to_string() } else { format!("COALESCE({}, 0)", updated.join(", ")) };
    let mut filters = vec!["1 = 1"];
    if has("archived") {
        filters.push("COALESCE(archived, 0) = 0");
    }
    if has("hidden") {
        filters.push("COALESCE(hidden, 0) = 0");
    }
    let sql = format!(
        "SELECT id, {source}, {cwd}, {title}, CAST({started} * 1000 AS INTEGER), CAST({updated} * 1000 AS INTEGER), \
         {parent} FROM sessions WHERE {filters} ORDER BY {updated} DESC LIMIT ?1",
        source = col("source", "NULL"),
        cwd = col("cwd", "NULL"),
        title = col("title", "NULL"),
        started = col("started_at", "0"),
        parent = col("parent_session_id", "NULL"),
        filters = filters.join(" AND "),
    );
    let mut stmt = conn.prepare(&sql)?;
    let sessions: Vec<Session> = stmt
        .query_map([limit as i64], |r| {
            let parent: Option<String> = r.get(6)?;
            let source: Option<String> = r.get(1)?;
            Ok(Session {
                id: r.get(0)?,
                entry: if parent.is_some_and(|p| !p.is_empty()) { Some("subagent".into()) } else { source },
                cwd: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                custom_title: r.get::<_, Option<String>>(3)?.filter(|t| !t.trim().is_empty()),
                started_ms: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                updated_ms: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                file: db.to_path_buf(),
                ..Default::default()
            })
        })?
        .collect::<Result<_, _>>()?;
    let mcols = sqlite::columns(&conn, "messages")?;
    let active = if mcols.contains("active") { "AND COALESCE(active, 1) = 1" } else { "" };
    let sql = format!(
        "SELECT content, {kind}, {meta} FROM messages WHERE session_id = ?1 AND role = 'user' {active} \
         ORDER BY {order}",
        kind = sqlite::col_or(&mcols, "display_kind", "NULL"),
        meta = sqlite::col_or(&mcols, "display_metadata", "NULL"),
        order = if mcols.contains("timestamp") { "timestamp, rowid" } else { "rowid" },
    );
    sessions.into_iter().map(|s| with_prompts(&conn, &sql, s, lim)).collect()
}

const INJECTED: [&str; 4] =
    ["[CONTEXT COMPACTION", "[CONTEXT SUMMARY]:", "[IMPORTANT: The user has invoked", "[System: "];

fn with_prompts(conn: &Connection, sql: &str, mut s: Session, lim: &Limits) -> rusqlite::Result<Session> {
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map([&s.id], |r| {
        Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?))
    })?;
    for (content, kind, meta) in rows.flatten() {
        let Some(content) = content else { continue };
        if kind.as_deref() == Some("hidden") {
            continue;
        }
        let model_only = meta.and_then(|m| serde_json::from_str::<Value>(&m).ok()).is_some_and(|m| {
            let f = &m["model_only"];
            f == true || f == 1
        });
        if model_only {
            continue;
        }
        // Multimodal turns are stored as a JSON array of parts.
        let text = match serde_json::from_str::<Value>(&content) {
            Ok(v @ Value::Array(_)) => text_of(&v),
            _ => content,
        };
        if INJECTED.iter().any(|p| text.trim_start().starts_with(p)) {
            continue;
        }
        s.push_prompt(&text, lim);
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_state_db() {
        let root = std::env::temp_dir().join(format!("chs-hermes-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        Connection::open(root.join("state.db"))
            .unwrap()
            .execute_batch(
                r#"
                CREATE TABLE sessions (id TEXT PRIMARY KEY, source TEXT, cwd TEXT, title TEXT, title_source TEXT,
                    started_at REAL, ended_at REAL, last_activity_at REAL, parent_session_id TEXT,
                    archived INTEGER DEFAULT 0, hidden INTEGER DEFAULT 0);
                CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id TEXT, role TEXT, content TEXT,
                    timestamp REAL, active INTEGER DEFAULT 1, display_kind TEXT, display_metadata TEXT);
                INSERT INTO sessions VALUES ('20261108_100000_ab','cli','/home/u/p','Fix tests','auto',1794132000.5,NULL,1794135600,NULL,0,0);
                INSERT INTO sessions VALUES ('20261108_110000_cd','oneshot','/home/u/p',NULL,NULL,1794135000,NULL,1794135000,NULL,0,0);
                INSERT INTO sessions VALUES ('20261107_old','cli','/x','Archived',NULL,1,NULL,2,NULL,1,0);
                INSERT INTO messages (session_id, role, content, timestamp, active, display_kind, display_metadata) VALUES
                  ('20261108_100000_ab','user','why do the tests fail',1,1,NULL,NULL),
                  ('20261108_100000_ab','assistant','looking',2,1,NULL,NULL),
                  ('20261108_100000_ab','user','rewound away',3,0,NULL,NULL),
                  ('20261108_100000_ab','user','[CONTEXT COMPACTION] earlier turns',4,1,NULL,NULL),
                  ('20261108_100000_ab','user','note for the model',5,1,NULL,'{"model_only":1}'),
                  ('20261108_100000_ab','user','[{"type":"text","text":"fix them"},{"type":"image_url"}]',6,1,NULL,NULL),
                  ('20261108_110000_cd','user','summarize the repo',1,1,NULL,NULL);
                "#,
            )
            .unwrap();
        let lim = Limits { max_prompt_chars: 4000, max_indexed_chars: 40000 };
        let v = load(&root, &lim, 10);
        let ids: Vec<&str> = v.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["20261108_100000_ab", "20261108_110000_cd"], "archived hidden, newest activity first");
        assert_eq!(v[0].prompts, vec!["why do the tests fail", "fix them"]);
        assert_eq!(v[0].title(), "Fix tests");
        assert_eq!(v[0].started_ms, 1794132000500);
        assert_eq!(v[1].entry.as_deref(), Some("oneshot"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
