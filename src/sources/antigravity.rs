//! Google Antigravity CLI (`agy`, closed source): `~/.gemini/antigravity-cli`.
//!
//! * `conversation_summaries.db`, table `conversation_summaries` - the CLI's own
//!   conversation index: conversation_id, title, preview, last_modified_time (text,
//!   `2026-10-03 16:43:44.002530482+00:00`), workspace_uris (a JSON array of
//!   `file://` URIs), parent_conversation_id, app_data_dir. The conversations
//!   themselves are protobuf blobs in `conversations/<id>.db` and are not read;
//! * `history.jsonl` - every prompt typed into the CLI, `{"display", "timestamp",
//!   "workspace", "conversationId"?}` (the id only from CLI 1.3).

use super::common::{jsonl, ms_of_str, uri_to_path};
use super::{sqlite, Reuse};
use crate::session::{Limits, Session};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

pub fn load(root: &Path, lim: &Limits, limit: usize, reuse: &Reuse) -> Vec<Session> {
    let db = root.join("conversation_summaries.db");
    let mut sessions = sqlite::with_db(&db, |conn| read(conn, &db, limit));
    // Prompts from the CLI's prompt history - which only grows - for the listed
    // conversations that changed since the last run, under the usual budget as they
    // stream past. When none changed, the history is not read at all.
    let at: HashMap<String, usize> =
        sessions.iter_mut().enumerate().filter_map(|(i, s)| (!reuse.fill(s)).then(|| (s.id.clone(), i))).collect();
    if at.is_empty() {
        return sessions;
    }
    if let Ok(f) = std::fs::File::open(root.join("history.jsonl")) {
        let _ = jsonl::<Value>(std::io::BufReader::new(f), &["\"conversationId\""], |v| {
            let i = v["conversationId"].as_str().and_then(|id| at.get(id));
            if let (Some(&i), Some(text)) = (i, v["display"].as_str()) {
                sessions[i].push_prompt(text, lim);
            }
        });
    }
    sessions
}

fn read(conn: &rusqlite::Connection, db: &Path, limit: usize) -> rusqlite::Result<Vec<Session>> {
    let cols = sqlite::columns(conn, "conversation_summaries")?;
    let col = |c: &'static str| sqlite::col_or(&cols, c, "NULL");
    let sql = format!(
        "SELECT conversation_id, {title}, {preview}, {modified}, {workspaces}, {parent}, {app} \
         FROM conversation_summaries ORDER BY {modified} DESC LIMIT ?1",
        title = col("title"),
        preview = col("preview"),
        modified = col("last_modified_time"),
        workspaces = col("workspace_uris"),
        parent = col("parent_conversation_id"),
        app = col("app_data_dir"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([limit as i64], |r| {
        let text = |i: usize| -> rusqlite::Result<Option<String>> {
            Ok(r.get::<_, Option<String>>(i)?.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
        };
        let cwd = text(4)?
            .and_then(|w| serde_json::from_str::<Vec<String>>(&w).ok())
            .and_then(|w| w.into_iter().next())
            .map(|u| uri_to_path(&u))
            .unwrap_or_default();
        let entry = if text(5)?.is_some() {
            Some("subagent".to_string())
        } else {
            // Rows of the desktop app's conversations, which the CLI can not resume.
            text(6)?.filter(|a| a != "antigravity-cli").map(|_| "app".to_string())
        };
        Ok(Session {
            id: r.get(0)?,
            ai_title: text(1)?,
            summary: text(2)?,
            updated_ms: text(3)?.and_then(|t| ms_of_str(&t)).unwrap_or(0),
            cwd,
            entry,
            file: db.to_path_buf(),
            ..Default::default()
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::common::testing::*;
    use rusqlite::Connection;

    #[test]
    fn reads_index_and_history() {
        let root = TempDir::new("agy");
        std::fs::create_dir_all(&root).unwrap();
        Connection::open(root.join("conversation_summaries.db"))
            .unwrap()
            .execute_batch(
                r#"
                CREATE TABLE conversation_summaries (conversation_id text PRIMARY KEY, title text, preview text,
                    step_count integer, last_modified_time datetime, workspace_uris text, status text, source text,
                    project_id text, agent_name text, parent_conversation_id text, nesting_depth integer,
                    last_user_input_time datetime, app_data_dir text);
                INSERT INTO conversation_summaries (conversation_id, title, preview, last_modified_time, workspace_uris,
                    parent_conversation_id, app_data_dir) VALUES
                  ('c1','Fix flaky test','fix the flaky test','2026-11-07 10:20:00.002530482+00:00','["file:///home/u/proj"]','',''),
                  ('c2','','sub task','2026-11-07 10:21:00+00:00','[]','c1','');
                "#,
            )
            .unwrap();
        std::fs::write(
            root.join("history.jsonl"),
            concat!(
                r#"{"display":"fix the flaky test","timestamp":1794046500000,"workspace":"/home/u/proj","conversationId":"c1"}"#,
                "\n",
                r#"{"display":"no id on old lines","timestamp":1794046500001,"workspace":"/home/u/proj"}"#,
                "\n",
                r#"{"display":"and the other one","timestamp":1794046600000,"workspace":"/home/u/proj","conversationId":"c1"}"#,
                "\n"
            ),
        )
        .unwrap();
        let lim = Limits::TEST;
        let v = load(&root, &lim, 10, &fresh(crate::config::Format::Antigravity));
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].id, "c2");
        assert_eq!(v[0].entry.as_deref(), Some("subagent"));
        assert_eq!(v[1].cwd, "/home/u/proj");
        assert_eq!(v[1].title(), "Fix flaky test");
        assert_eq!(v[1].prompts, vec!["fix the flaky test", "and the other one"]);
        assert!(v[1].updated_ms > 0);
    }
}
