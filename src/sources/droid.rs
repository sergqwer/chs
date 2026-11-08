//! Factory Droid: `~/.factory` (or `$FACTORY_HOME_OVERRIDE/.factory`), one JSONL per
//! session, `sessions/<project key>/<uuid>.jsonl` (`/btw` side chats under
//! `sessions/btw/`). Appended to, except that the first line - the
//! `{"type":"session_start","id","title","cwd","lastCwd"?,"callingSessionId"?}`
//! summary - is rewritten in place when the title or folder changes. That edit can
//! fall anywhere in a long first line, so these files are read whole when they change
//! (the substring prefilter keeps that cheap).
//!
//! A prompt is `{"type":"message","message":{"role":"user","content":...}}`, only its
//! text parts (tool results ride in user messages too). Hook output, hidden or
//! model-only messages are skipped; `userMessageSource` tells a person (`cli_tui`,
//! `desktop`, `web`) from `droid exec` (`cli_exec`) and automations.

use super::common::{jsonl, ms_of, str_of, text_of, walk};
use crate::session::{Limits, Session};
use serde_json::Value;
use std::io::BufRead;
use std::path::{Path, PathBuf};

pub fn list_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(&root.join("sessions"), 1, &|n| n.ends_with(".jsonl"), &mut out);
    out
}

const NEEDLES: [&str; 3] = ["\"session_start\"", "\"role\":\"user\"", "\"role\": \"user\""];

pub fn parse(reader: impl BufRead, s: &mut Session, lim: &Limits) -> std::io::Result<u64> {
    jsonl::<Value>(reader, &NEEDLES, |v| apply(&v, s, lim))
}

pub fn parse_file(path: &Path, s: &mut Session, lim: &Limits) -> std::io::Result<()> {
    let f = std::fs::File::open(path)?;
    parse(std::io::BufReader::with_capacity(256 * 1024, f), s, lim).map(|_| ())
}

fn apply(v: &Value, s: &mut Session, lim: &Limits) {
    match v["type"].as_str() {
        Some("session_start") => {
            if let Some(id) = str_of(&v["id"]) {
                s.id = id;
            }
            let title = str_of(&v["title"]).or_else(|| str_of(&v["sessionTitle"]));
            if v["isSessionTitleManuallySet"] == true {
                s.custom_title = title;
            } else {
                s.ai_title = title;
            }
            // Droid itself resumes in lastCwd when there is one.
            if let Some(cwd) = str_of(&v["lastCwd"]).or_else(|| str_of(&v["cwd"])) {
                s.cwd = cwd;
            }
            if str_of(&v["callingSessionId"]).is_some() {
                s.entry = Some("subagent".into());
            }
        }
        Some("message") if v["message"]["role"] == "user" => {
            let m = &v["message"];
            if m.get("hookEventName").is_some_and(|h| !h.is_null())
                || m["hiddenFromUserViews"] == true
                || m["visibility"] == "llm_only"
            {
                return;
            }
            match m["userMessageSource"].as_str() {
                None | Some("cli_tui" | "desktop" | "web") => {}
                Some("cli_exec") => {
                    s.entry.get_or_insert_with(|| "exec".into());
                }
                Some(_) => {
                    // automation, wiki-*, readiness-*: generated, not typed.
                    s.entry.get_or_insert_with(|| "automation".into());
                    return;
                }
            }
            if s.started_ms == 0 {
                s.started_ms = ms_of(&v["timestamp"]).unwrap_or(0);
            }
            s.push_prompt(&text_of(&m["content"]), lim);
        }
        _ => {}
    }
}

/// `/btw` side chats are kept apart from real sessions.
pub fn finish(s: &mut Session) {
    let side = s.file.parent().and_then(|p| p.file_name()).is_some_and(|n| n == "btw");
    if side && s.entry.is_none() {
        s.entry = Some("btw".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_session() {
        let text = [
            r#"{"type":"session_start","id":"2b1e","title":"Fix failing test","owner":"u","version":2,"cwd":"/home/u/proj","lastCwd":"/home/u/proj/sub"}"#,
            r#"{"type":"message","id":"m1","timestamp":"2026-11-08T10:00:05.000Z","message":{"role":"user","content":[{"type":"text","text":"fix the failing test"}],"userMessageSource":"cli_tui"}}"#,
            r#"{"type":"message","id":"m2","timestamp":"2026-11-08T10:00:20.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Done."}]}}"#,
            r#"{"type":"message","id":"m3","timestamp":"2026-11-08T10:00:30.000Z","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"message","id":"m4","timestamp":"2026-11-08T10:00:40.000Z","message":{"role":"user","content":"hook says","hookEventName":"PostToolUse"}}"#,
            r#"{"type":"message","id":"m5","timestamp":"2026-11-08T10:02:00.000Z","message":{"role":"user","content":[{"type":"text","text":"now add a changelog entry"}]}}"#,
            "",
        ]
        .join("\n");
        let lim = Limits { max_prompt_chars: 4000, max_indexed_chars: 40000 };
        let mut s = Session::default();
        parse(text.as_bytes(), &mut s, &lim).unwrap();
        assert_eq!(s.id, "2b1e");
        assert_eq!(s.cwd, "/home/u/proj/sub");
        assert_eq!(s.prompts, vec!["fix the failing test", "now add a changelog entry"]);
        assert_eq!(s.title(), "Fix failing test");
        assert_eq!(s.entry, None);

        let mut e = Session::default();
        let exec = r#"{"type":"message","message":{"role":"user","content":"run it","userMessageSource":"cli_exec"}}"#;
        parse(format!("{exec}\n").as_bytes(), &mut e, &lim).unwrap();
        assert_eq!(e.entry.as_deref(), Some("exec"));
        assert_eq!(e.prompts, vec!["run it"]);
    }
}
