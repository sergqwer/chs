//! Amazon Q Developer CLI (`q chat`, open source until it became the closed Kiro CLI):
//! `<local data>/amazon-q/data.sqlite3`, table `conversations(key, value)` where the
//! key is the absolute folder and the value the JSON conversation state - so there is
//! one conversation per folder, overwritten every turn, and `q chat --resume` in that
//! folder picks it up. Prompts are `history[].user.content.Prompt.prompt`; tool results
//! are other `content` variants.

use super::common::ms_of;
use super::sqlite;
use crate::session::{Limits, Session};
use serde_json::Value;
use std::path::Path;

pub fn load(root: &Path, lim: &Limits, limit: usize) -> Vec<Session> {
    let db = root.join("data.sqlite3");
    if !db.is_file() {
        return Vec::new();
    }
    read(&db, lim, limit).unwrap_or_default()
}

fn read(db: &Path, lim: &Limits, limit: usize) -> rusqlite::Result<Vec<Session>> {
    let conn = sqlite::open(db)?;
    if !sqlite::tables(&conn)?.contains("conversations") {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare("SELECT key, value FROM conversations")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    // Rows carry no time of their own; prompts usually do, the file is the fallback.
    let fallback = std::fs::metadata(db)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64);
    let mut out = Vec::new();
    for (key, value) in rows.flatten() {
        let Ok(v) = serde_json::from_str::<Value>(&value) else { continue };
        let mut s = Session {
            id: v["conversation_id"].as_str().unwrap_or(&key).to_string(),
            cwd: key,
            file: db.to_path_buf(),
            ..Default::default()
        };
        for turn in v["history"].as_array().into_iter().flatten() {
            let user = turn.get("user").unwrap_or(turn);
            if let Some(ms) = ms_of(&user["timestamp"]) {
                if s.started_ms == 0 {
                    s.started_ms = ms;
                }
                s.updated_ms = ms;
            }
            let c = &user["content"];
            let text = c["Prompt"]["prompt"].as_str().or_else(|| c["CancelledToolUses"]["prompt"].as_str());
            if let Some(t) = text {
                s.push_prompt(t, lim);
            }
        }
        if s.updated_ms == 0 {
            s.updated_ms = fallback;
        }
        out.push(s);
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
    out.truncate(limit);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn reads_conversations() {
        let root = std::env::temp_dir().join(format!("chs-q-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let value = serde_json::json!({
            "conversation_id": "c-1",
            "history": [
                {"user": {"content": {"Prompt": {"prompt": "list the files"}}, "timestamp": "2026-11-08T10:00:00Z"},
                 "assistant": {"Response": {"content": "3"}}},
                {"user": {"content": {"ToolUseResults": {"tool_use_results": []}}, "timestamp": "2026-11-08T10:00:01Z"}},
                {"user": {"content": {"Prompt": {"prompt": "add a README"}}, "timestamp": "2026-11-08T10:05:00Z"}}
            ]
        });
        let conn = Connection::open(root.join("data.sqlite3")).unwrap();
        conn.execute_batch("CREATE TABLE conversations (key TEXT PRIMARY KEY, value TEXT)").unwrap();
        conn.execute("INSERT INTO conversations VALUES (?1, ?2)", ["/home/u/proj", &value.to_string()]).unwrap();
        drop(conn);
        let lim = Limits { max_prompt_chars: 4000, max_indexed_chars: 40000 };
        let v = load(&root, &lim, 10);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, "c-1");
        assert_eq!(v[0].cwd, "/home/u/proj");
        assert_eq!(v[0].prompts, vec!["list the files", "add a README"]);
        assert!(v[0].updated_ms > v[0].started_ms);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
