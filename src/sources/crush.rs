//! Crush (charmbracelet/crush). Every project has its own database,
//! `<project>/.crush/crush.db`; the global data folder (`~/.local/share/crush`,
//! `%LOCALAPPDATA%\crush` on Windows) holds `projects.json` listing them:
//! `{"projects": [{"path", "data_dir", "last_accessed"}]}` with absolute paths.
//!
//! * `sessions` - id, parent_session_id (subagent and title-generation children),
//!   title ("Untitled Session" until named), created_at/updated_at in **seconds**;
//! * `messages` - role user|assistant|system|tool, `parts` = JSON array of
//!   `{"type": "text", "data": {"text": ..., "hidden": bool}}` and other part types.
//!   A user message carrying `shell_command` or `sub_agent_report` parts is tool
//!   plumbing, not something the user typed.

use super::{sqlite, Reuse};
use crate::session::{Limits, Session};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct ProjectList {
    #[serde(default)]
    projects: Vec<Project>,
}

#[derive(Deserialize)]
struct Project {
    path: String,
    #[serde(default)]
    data_dir: String,
}

pub fn load(root: &Path, lim: &Limits, limit: usize, reuse: &Reuse) -> Vec<Session> {
    let Ok(text) = std::fs::read_to_string(root.join("projects.json")) else { return Vec::new() };
    let Ok(list) = serde_json::from_str::<ProjectList>(&text) else { return Vec::new() };
    // The rows of every project first; prompts only for the newest `limit` of them all.
    let mut conns: HashMap<PathBuf, Connection> = HashMap::new();
    let mut out = Vec::new();
    for p in list.projects {
        let data = if p.data_dir.is_empty() { Path::new(&p.path).join(".crush") } else { PathBuf::from(&p.data_dir) };
        let db = data.join("crush.db");
        let Ok(conn) = (if db.is_file() { sqlite::open(&db) } else { continue }) else { continue };
        if let Ok(rows) = sessions(&conn, &db, &p.path, limit) {
            out.extend(rows);
            conns.insert(db, conn);
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
    out.truncate(limit);
    for s in &mut out {
        if !reuse.fill(s) {
            if let Some(conn) = conns.get(&s.file) {
                let _ = prompts(conn, s, lim);
            }
        }
    }
    out
}

fn sessions(conn: &Connection, db: &Path, project: &str, limit: usize) -> rusqlite::Result<Vec<Session>> {
    let cols = sqlite::columns(conn, "sessions")?;
    let parent = sqlite::col_or(&cols, "parent_session_id", "NULL");
    let sql =
        format!("SELECT id, title, created_at, updated_at, {parent} FROM sessions ORDER BY updated_at DESC LIMIT ?1");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([limit as i64], |r| {
        let title: Option<String> = r.get(1)?;
        let parent: Option<String> = r.get(4)?;
        Ok(Session {
            id: r.get(0)?,
            cwd: project.to_string(),
            custom_title: title.filter(|t| !t.trim().is_empty() && t != "Untitled Session"),
            started_ms: r.get::<_, Option<i64>>(2)?.unwrap_or(0) * 1000,
            updated_ms: r.get::<_, Option<i64>>(3)?.unwrap_or(0) * 1000,
            entry: parent.map(|_| "subagent".to_string()),
            file: db.to_path_buf(),
            ..Default::default()
        })
    })?;
    rows.collect()
}

fn prompts(conn: &Connection, s: &mut Session, lim: &Limits) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare_cached(
        "SELECT parts FROM messages WHERE session_id = ?1 AND role = 'user' ORDER BY created_at, rowid",
    )?;
    for parts in stmt.query_map([&s.id], |r| r.get::<_, String>(0))?.flatten() {
        if let Some(t) = user_text(&parts) {
            s.push_prompt(&t, lim);
        }
    }
    Ok(())
}

fn user_text(parts: &str) -> Option<String> {
    let parts: Vec<Value> = serde_json::from_str(parts).ok()?;
    fn kind(p: &Value) -> &str {
        p.get("type").and_then(Value::as_str).unwrap_or("")
    }
    if parts.iter().any(|p| matches!(kind(p), "shell_command" | "sub_agent_report")) {
        return None;
    }
    let texts: Vec<&str> = parts
        .iter()
        .filter(|p| kind(p) == "text" && p["data"]["hidden"].as_bool() != Some(true))
        .filter_map(|p| p["data"]["text"].as_str())
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::common::testing::*;

    #[test]
    fn reads_projects_and_sessions() {
        let base = TempDir::new("crush");
        let proj = base.join("proj");
        let data = proj.join(".crush");
        std::fs::create_dir_all(&data).unwrap();
        let global = base.join("global");
        std::fs::create_dir_all(&global).unwrap();
        let list = serde_json::json!({"projects": [{"path": proj, "data_dir": data, "last_accessed": "2026-11-01T10:00:00Z"}]});
        std::fs::write(global.join("projects.json"), list.to_string()).unwrap();
        Connection::open(data.join("crush.db"))
            .unwrap()
            .execute_batch(
                r#"
                CREATE TABLE sessions (id TEXT PRIMARY KEY, parent_session_id TEXT, title TEXT, message_count INTEGER,
                    created_at INTEGER, updated_at INTEGER);
                CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT, role TEXT, parts TEXT, created_at INTEGER);
                INSERT INTO sessions VALUES ('s1', NULL, 'Untitled Session', 3, 1000, 2000);
                INSERT INTO sessions VALUES ('t1', 's1', 'Generate a title', 1, 1000, 1500);
                INSERT INTO messages VALUES ('m1','s1','user','[{"type":"text","data":{"text":"refactor the db layer"}}]',1001);
                INSERT INTO messages VALUES ('m2','s1','assistant','[{"type":"text","data":{"text":"ok"}}]',1002);
                INSERT INTO messages VALUES ('m3','s1','user','[{"type":"shell_command","data":{"command":"ls"}}]',1003);
                INSERT INTO messages VALUES ('m4','s1','user','[{"type":"text","data":{"text":"continue","hidden":true}}]',1004);
                INSERT INTO messages VALUES ('m5','s1','user','[{"type":"text","data":{"text":"and add tests"}}]',1005);
                "#,
            )
            .unwrap();
        let lim = Limits::TEST;
        let v = load(&global, &lim, 10, &fresh(crate::config::Format::Crush));
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].id, "s1");
        assert_eq!(v[0].updated_ms, 2_000_000);
        assert_eq!(v[0].cwd, proj.to_string_lossy());
        assert_eq!(v[0].prompts, vec!["refactor the db layer", "and add tests"]);
        assert_eq!(v[0].title(), "refactor the db layer");
        assert_eq!(v[1].entry.as_deref(), Some("subagent"));
    }
}
