//! Qwen Code (0.4 and later): `~/.qwen` (`$QWEN_RUNTIME_DIR`, `$QWEN_HOME`),
//! `projects/<sanitized cwd>/chats/<session id>.jsonl` (archived chats in
//! `chats/archive/` are left out). Append-only JSONL in Claude Code's style - every
//! record carries `sessionId`, `cwd`, `gitBranch`, `timestamp` - with Gemini-style
//! message bodies (`message.parts`).
//!
//! A prompt is `type: "user"` with `provenance: "real_user"` and no `subtype` (cron
//! runs, notifications, agent messages are users too); what the person typed is
//! `systemPayload.displayText`, falling back to the parts. A `system` record with
//! `subtype: "custom_title"` names the session.

use super::common::{jsonl, ms_of, str_of, text_of};
use crate::session::{Limits, Session};
use serde_json::Value;
use std::io::BufRead;
use std::path::{Path, PathBuf};

pub fn list_files(root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(root.join("projects")) else { return Vec::new() };
    let mut out = Vec::new();
    for project in rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) {
        super::common::walk(&project.path().join("chats"), 0, &|n| n.ends_with(".jsonl"), &mut out);
    }
    out
}

const NEEDLES: [&str; 3] = ["\"type\":\"user\"", "\"type\": \"user\"", "\"custom_title\""];

pub fn parse(reader: impl BufRead, s: &mut Session, lim: &Limits) -> std::io::Result<u64> {
    jsonl::<Value>(reader, &NEEDLES, |v| apply(&v, s, lim))
}

fn apply(v: &Value, s: &mut Session, lim: &Limits) {
    match v["type"].as_str() {
        Some("user") => {
            if v.get("subtype").is_some_and(|x| !x.is_null())
                || v.get("provenance").and_then(Value::as_str).is_some_and(|p| p != "real_user")
            {
                return;
            }
            if s.id.is_empty() {
                s.id = str_of(&v["sessionId"]).unwrap_or_default();
            }
            if s.cwd.is_empty() {
                s.cwd = str_of(&v["cwd"]).unwrap_or_default();
            }
            if s.started_ms == 0 {
                s.started_ms = ms_of(&v["timestamp"]).unwrap_or(0);
            }
            if let Some(b) = str_of(&v["gitBranch"]).filter(|b| b != "HEAD") {
                s.branch = Some(b);
            }
            let text = str_of(&v["systemPayload"]["displayText"]).unwrap_or_else(|| text_of(&v["message"]["parts"]));
            s.push_prompt(&text, lim);
        }
        Some("system") if v["subtype"] == "custom_title" => {
            let p = &v["systemPayload"];
            let title = str_of(&p["customTitle"]);
            if p["titleSource"] == "manual" {
                s.custom_title = title;
            } else {
                s.ai_title = title;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_session() {
        let text = [
            r#"{"uuid":"u1","sessionId":"S1","timestamp":"2026-11-08T10:00:05.000Z","type":"user","provenance":"real_user","cwd":"/home/u/proj","gitBranch":"main","message":{"role":"user","parts":[{"text":"list the files here"}]},"systemPayload":{"displayText":"list the files here","hookContext":""}}"#,
            r#"{"uuid":"a1","sessionId":"S1","type":"assistant","cwd":"/home/u/proj","message":{"role":"model","parts":[{"text":"There are 3 files."}]}}"#,
            r#"{"uuid":"s1","sessionId":"S1","type":"system","subtype":"custom_title","systemPayload":{"customTitle":"List files","titleSource":"auto"}}"#,
            r#"{"uuid":"c1","sessionId":"S1","type":"user","subtype":"cron","provenance":"real_user","message":{"role":"user","parts":[{"text":"cron tick"}]}}"#,
            r#"{"uuid":"u2","sessionId":"S1","type":"user","provenance":"mod_prompt","message":{"role":"user","parts":[{"text":"injected"}]}}"#,
            r#"{"uuid":"u3","sessionId":"S1","type":"user","provenance":"real_user","cwd":"/home/u/proj","message":{"role":"user","parts":[{"text":"now add a README"}]}}"#,
            "",
        ]
        .join("\n");
        let lim = Limits { max_prompt_chars: 4000, max_indexed_chars: 40000 };
        let mut s = Session::default();
        parse(text.as_bytes(), &mut s, &lim).unwrap();
        assert_eq!(s.id, "S1");
        assert_eq!(s.cwd, "/home/u/proj");
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.prompts, vec!["list the files here", "now add a README"]);
        assert_eq!(s.title(), "List files");
    }
}
