//! Codex CLI: `<root>/sessions/YYYY/MM/DD/rollout-<timestamp>-<id>.jsonl`.
//!
//! Every line is `{"timestamp", "type", "payload"}`. The user's prompts appear as
//! `event_msg/user_message` (older writers) or `event_msg/item_completed` with a
//! `UserMessage` item (newer ones); raw `response_item` user messages are kept as a
//! fallback because they also carry injected context blocks that have to be filtered.

use crate::session::{parse_ts, Limits, Session};
use memchr::memmem;
use serde::Deserialize;
use serde_json::Value;
use std::io::BufRead;
use std::path::{Path, PathBuf};

pub fn list_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(&root.join("sessions"), 0, &mut out);
    out
}

fn walk(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() && depth < 4 {
            walk(&p, depth + 1, out);
        } else if ft.is_file()
            && p.extension().is_some_and(|x| x == "jsonl")
            && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("rollout-"))
        {
            out.push(p);
        }
    }
}

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    payload: Value,
}

// Quoted values only, so JSON spacing around ':' does not matter; the role needle
// comes in both spellings for the same reason.
const NEEDLES: [&[u8]; 5] =
    [b"\"session_meta\"", b"\"user_message\"", b"\"UserMessage\"", b"\"role\":\"user\"", b"\"role\": \"user\""];

pub fn parse(mut reader: impl BufRead, s: &mut Session, lim: &Limits) -> std::io::Result<u64> {
    let finders: Vec<memmem::Finder> = NEEDLES.iter().map(memmem::Finder::new).collect();
    let mut buf = Vec::with_capacity(64 * 1024);
    let mut consumed = 0u64;
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            break;
        }
        consumed += n as u64;
        if !finders.iter().any(|f| f.find(&buf).is_some()) {
            continue;
        }
        let Ok(line) = serde_json::from_slice::<Line>(&buf) else { continue };
        apply(line, s, lim);
    }
    Ok(consumed)
}

fn apply(line: Line, s: &mut Session, lim: &Limits) {
    let p = &line.payload;
    match line.ty.as_str() {
        "session_meta" => {
            if s.id.is_empty() {
                if let Some(id) = p.get("id").or_else(|| p.get("session_id")).and_then(Value::as_str) {
                    s.id = id.to_string();
                }
            }
            if s.cwd.is_empty() {
                if let Some(c) = p.get("cwd").and_then(Value::as_str) {
                    s.cwd = c.to_string();
                }
            }
            if s.started_ms == 0 {
                if let Some(ms) = p.get("timestamp").and_then(Value::as_str).and_then(parse_ts) {
                    s.started_ms = ms;
                }
            }
            if s.entry.is_none() {
                s.entry = match p.get("source") {
                    Some(Value::String(v)) => Some(v.clone()),
                    // {"subagent": "review"} and similar: spawned by another agent.
                    Some(Value::Object(m)) => m.keys().next().cloned(),
                    _ => None,
                };
            }
            if s.branch.is_none() {
                if let Some(b) = p.pointer("/git/branch").and_then(Value::as_str) {
                    s.branch = Some(b.to_string());
                }
            }
        }
        "event_msg" => match p.get("type").and_then(Value::as_str) {
            Some("user_message") => {
                if let Some(m) = p.get("message").and_then(Value::as_str) {
                    if let Some(t) = clean_prompt(m) {
                        s.push_prompt(&t, lim);
                    }
                }
            }
            Some("item_completed") => {
                let item = &p["item"];
                if item.get("type").and_then(Value::as_str) == Some("UserMessage") {
                    let text = join_text(&item["content"]);
                    if let Some(t) = clean_prompt(&text) {
                        s.push_prompt(&t, lim);
                    }
                }
            }
            _ => {}
        },
        "response_item"
            if p.get("type").and_then(Value::as_str) == Some("message")
                && p.get("role").and_then(Value::as_str) == Some("user") =>
        {
            for part in p["content"].as_array().into_iter().flatten() {
                if let Some(t) = part.get("text").and_then(Value::as_str).and_then(clean_prompt) {
                    s.push_alt_prompt(&t, lim);
                }
            }
        }
        _ => {}
    }
}

fn join_text(content: &Value) -> String {
    let mut out = String::new();
    for part in content.as_array().into_iter().flatten() {
        let ty = part.get("type").and_then(Value::as_str).unwrap_or("");
        if ty == "text" || ty == "input_text" {
            if let Some(t) = part.get("text").and_then(Value::as_str) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
        }
    }
    out
}

/// Context Codex injects as "user" text: environment, AGENTS.md, permissions, etc.
fn clean_prompt(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty()
        || t.starts_with("<environment_context>")
        || t.starts_with("<user_instructions>")
        || t.starts_with("<permissions")
        || t.starts_with("<turn_aborted>")
        || t.starts_with("<user_shell_command>")
        || t.starts_with("# AGENTS.md")
    {
        return None;
    }
    Some(t.to_string())
}

/// Called after a parse: use the fallback prompts only if no explicit ones exist.
pub fn finish(s: &mut Session, lim: &Limits) {
    if s.prompts.is_empty() && !s.alt_prompts.is_empty() {
        for p in std::mem::take(&mut s.alt_prompts) {
            s.push_prompt(&p, lim);
        }
    } else if !s.prompts.is_empty() {
        s.alt_prompts.clear();
    }
    s.alt_chars = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIM: Limits = Limits { max_prompt_chars: 4000, max_indexed_chars: 40000 };

    fn run(lines: &[&str]) -> Session {
        let data = lines.iter().map(|l| format!("{l}\n")).collect::<String>();
        let mut s = Session::default();
        parse(data.as_bytes(), &mut s, &LIM).unwrap();
        finish(&mut s, &LIM);
        s
    }

    #[test]
    fn old_format_events() {
        let s = run(&[
            r#"{"timestamp":"2026-06-23T22:56:43.058Z","type":"session_meta","payload":{"id":"019e","timestamp":"2026-06-23T22:56:42.987Z","cwd":"/tmp/x","originator":"codex_exec","source":"exec","git":{"branch":"dev"}}}"#,
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n<cwd>/tmp</cwd></environment_context>"}]}}"#,
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Review the patch"}]}}"#,
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"user_message","message":"Review the patch","images":[]}}"#,
        ]);
        assert_eq!(s.id, "019e");
        assert_eq!(s.cwd, "/tmp/x");
        assert_eq!(s.entry.as_deref(), Some("exec"));
        assert_eq!(s.branch.as_deref(), Some("dev"));
        assert_eq!(s.prompts, vec!["Review the patch"]);
        assert!(s.alt_prompts.is_empty());
    }

    #[test]
    fn new_format_items_and_object_source() {
        let s = run(&[
            r#"{"timestamp":"2026-11-07T12:40:57.976Z","type":"session_meta","payload":{"id":"01a1","cwd":"/w","source":{"subagent":"review"}}}"#,
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","id":"u1","content":[{"type":"local_image","path":"/x.png"},{"type":"text","text":"look at this screenshot"}]}}}"#,
        ]);
        assert_eq!(s.entry.as_deref(), Some("subagent"));
        assert_eq!(s.prompts, vec!["look at this screenshot"]);
    }

    #[test]
    fn falls_back_to_response_items() {
        let s = run(&[
            r#"{"timestamp":"t","type":"session_meta","payload":{"id":"z","cwd":"/w","source":"cli"}}"#,
            r##"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /w"}]}}"##,
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello codex"}]}}"#,
        ]);
        assert_eq!(s.prompts, vec!["hello codex"]);
    }
}
