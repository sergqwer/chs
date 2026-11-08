//! Gemini CLI: `~/.gemini` (`$GEMINI_CLI_HOME/.gemini`), one folder per project,
//! `tmp/<project slug>/chats/session-<time>-<id8>.jsonl`, with the project's real path
//! in `tmp/<slug>/.project_root`. Subagent chats sit one level deeper and are skipped.
//!
//! Since 0.39 a session is append-only JSONL: a first metadata line `{"sessionId",
//! "startTime", "kind"}`, message records `{"id", "type": "user"|"gemini"|...,
//! "content", "displayContent"?}`, and `{"$set": {...}}` updates (`summary` is the
//! generated title). Older builds wrote the same record as one `session-*.json` file,
//! `{"sessionId", "startTime", "summary", "messages": [...]}`, rewritten every turn.
//!
//! Tool results are recorded as `type: "user"` too (their parts carry a
//! `functionResponse`); like Gemini's own session list, commands (`/`, `?`) and
//! injected `<session_context>` / `<hook_context>` blocks are not prompts.

use super::common::{jsonl, ms_of, read_json, str_of, subdirs, text_of, walk, Found};
use crate::session::{Limits, Session};
use serde_json::Value;
use std::io::BufRead;
use std::path::Path;

pub fn list_files(root: &Path) -> Vec<Found> {
    let keep = |n: &str| n.starts_with("session-") && (n.ends_with(".jsonl") || n.ends_with(".json"));
    let mut out = Vec::new();
    for project in subdirs(&root.join("tmp")) {
        walk(&project.join("chats"), 0, &keep, &mut out);
    }
    out
}

/// JSONL is appended to; the old single-JSON files are rewritten.
pub fn incremental(path: &Path) -> bool {
    path.extension().is_some_and(|x| x == "jsonl")
}

const NEEDLES: [&str; 5] = ["\"sessionId\"", "\"$set\"", "\"type\":\"user\"", "\"type\": \"user\"", "\"messages\""];

pub fn parse(reader: impl BufRead, s: &mut Session, lim: &Limits) -> std::io::Result<u64> {
    jsonl::<Value>(reader, &NEEDLES, |v| apply(&v, s, lim))
}

pub fn parse_file(path: &Path, s: &mut Session, lim: &Limits) -> std::io::Result<()> {
    let v: Value = read_json(path)?;
    meta(&v, s);
    if let Some(t) = str_of(&v["summary"]) {
        s.ai_title = Some(t);
    }
    for m in v["messages"].as_array().into_iter().flatten() {
        message(m, s, lim);
    }
    Ok(())
}

fn apply(v: &Value, s: &mut Session, lim: &Limits) {
    if let Some(set) = v.get("$set") {
        if let Some(t) = str_of(&set["summary"]) {
            s.ai_title = Some(t);
        }
        // A legacy full replacement of the history.
        if let Some(list) = set["messages"].as_array() {
            s.prompts.clear();
            s.prompt_chars = 0;
            s.prompt_count = 0;
            for m in list {
                message(m, s, lim);
            }
        }
    } else if v.get("type").is_some() {
        message(v, s, lim);
    } else if v.get("sessionId").is_some() {
        meta(v, s);
    }
}

fn meta(v: &Value, s: &mut Session) {
    if let Some(id) = str_of(&v["sessionId"]) {
        s.id = id;
    }
    if let Some(ms) = ms_of(&v["startTime"]) {
        s.started_ms = ms;
    }
    if v["kind"] == "subagent" {
        s.entry = Some("subagent".into());
    }
}

fn message(m: &Value, s: &mut Session, lim: &Limits) {
    if m["type"] != "user" {
        return;
    }
    let parts = m["content"].as_array();
    if parts.is_some_and(|p| p.iter().any(|x| x.get("functionResponse").is_some())) {
        return;
    }
    let shown = m.get("displayContent").filter(|d| !d.is_null()).unwrap_or(&m["content"]);
    let text = text_of(shown);
    let t = text.trim_start();
    if t.starts_with('/') || t.starts_with('?') || t.starts_with("<session_context>") || t.starts_with("<hook_context>")
    {
        return;
    }
    s.push_prompt(&text, lim);
}

/// The project folder, from the `.project_root` marker of the chat's project. Runs
/// with the parse, so the result is cached with it.
pub fn locate(path: &Path, s: &mut Session) {
    if !s.cwd.is_empty() {
        return;
    }
    let marker = path.parent().and_then(Path::parent).map(|p| p.join(".project_root"));
    if let Some(root) = marker.and_then(|m| std::fs::read_to_string(m).ok()) {
        s.cwd = root.trim().to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::common::testing::*;

    const LIM: Limits = Limits::TEST;

    #[test]
    fn reads_jsonl_session() {
        let text = [
            r#"{"sessionId":"3f2a","projectHash":"9f86","startTime":"2026-11-08T10:00:00.000Z","lastUpdated":"2026-11-08T10:00:00.000Z","kind":"main"}"#,
            r#"{"id":"m1","timestamp":"2026-11-08T10:00:05.000Z","type":"user","content":[{"text":"list the files here"}]}"#,
            r#"{"$set":{"lastUpdated":"2026-11-08T10:00:05.000Z"}}"#,
            r#"{"id":"m2","timestamp":"2026-11-08T10:00:09.000Z","type":"gemini","content":[{"text":"There are 3 files."}]}"#,
            r#"{"id":"m3","type":"user","content":[{"functionResponse":{"name":"ls","response":{}}}]}"#,
            r#"{"id":"m4","type":"user","content":[{"text":"<session_context>\ncwd: /home/u/proj\n</session_context>"}]}"#,
            r#"{"id":"m5","type":"user","content":[{"text":"/stats"}]}"#,
            r#"{"id":"m6","type":"user","content":[{"text":"now add a README @x.md expanded"}],"displayContent":[{"text":"now add a README @x.md"}]}"#,
            r#"{"$set":{"summary":"List files and add README"}}"#,
            "",
        ]
        .join("\n");
        let mut s = Session::default();
        parse(text.as_bytes(), &mut s, &LIM).unwrap();
        assert_eq!(s.id, "3f2a");
        assert_eq!(s.prompts, vec!["list the files here", "now add a README @x.md"]);
        assert_eq!(s.title(), "List files and add README");
        assert!(s.started_ms > 0);
    }

    #[test]
    fn reads_legacy_json_and_project_root() {
        let root = TempDir::new("gem");
        let chats = root.join("tmp/proj/chats");
        std::fs::create_dir_all(&chats).unwrap();
        std::fs::write(root.join("tmp/proj/.project_root"), "/home/u/proj\n").unwrap();
        let f = chats.join("session-2026-03-01T10-00-3f2a.json");
        std::fs::write(&f, r#"{"sessionId":"old1","startTime":"2026-03-01T10:00:00Z","messages":[{"id":"a","type":"user","content":"hello"}]}"#).unwrap();
        assert_eq!(paths(list_files(&root)), vec![f.clone()]);
        assert!(!incremental(&f));
        let mut s = Session::default();
        parse_file(&f, &mut s, &LIM).unwrap();
        locate(&f, &mut s);
        assert_eq!(s.id, "old1");
        assert_eq!(s.prompts, vec!["hello"]);
        assert_eq!(s.cwd, "/home/u/proj");
    }
}
