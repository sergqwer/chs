//! Tencent CodeBuddy Code: `~/.codebuddy` (`$CODEBUDDY_CONFIG_DIR`),
//! `projects/<compressed cwd>/<session id>.jsonl` (subagents one level deeper).
//! Append-only JSONL of OpenAI Agents SDK items, each with `cwd` and a ms `timestamp`:
//! a prompt is `{"type": "message", "role": "user", "content": [{"type":
//! "input_text"}]}` unless `providerData` marks it as meta / additional data, and the
//! title comes from Claude-style `custom-title` / `ai-title` lines or the
//! `codebuddy.ai/sessionTitle` key of a `session-meta` line.

use super::common::{jsonl, ms_of, str_of, text_of};
use crate::session::{Limits, Session};
use serde_json::Value;
use std::io::BufRead;
use std::path::{Path, PathBuf};

pub fn list_files(root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(root.join("projects")) else { return Vec::new() };
    let mut out = Vec::new();
    for project in rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) {
        super::common::walk(&project.path(), 0, &|n| n.ends_with(".jsonl"), &mut out);
    }
    out
}

const NEEDLES: [&str; 5] =
    ["\"role\":\"user\"", "\"role\": \"user\"", "\"custom-title\"", "\"ai-title\"", "\"session-meta\""];

pub fn parse(reader: impl BufRead, s: &mut Session, lim: &Limits) -> std::io::Result<u64> {
    jsonl::<Value>(reader, &NEEDLES, |v| apply(&v, s, lim))
}

fn apply(v: &Value, s: &mut Session, lim: &Limits) {
    match v["type"].as_str() {
        Some("message") if v["role"] == "user" => {
            let pd = &v["providerData"];
            if pd["isMeta"] == true || pd["skipRun"] == true || pd["isAdditionalData"] == true {
                return;
            }
            if s.cwd.is_empty() {
                s.cwd = str_of(&v["cwd"]).unwrap_or_default();
            }
            if s.started_ms == 0 {
                s.started_ms = ms_of(&v["timestamp"]).unwrap_or(0);
            }
            s.push_prompt(&text_of(&v["content"]), lim);
        }
        Some("custom-title") => s.custom_title = str_of(&v["customTitle"]),
        Some("ai-title") => s.ai_title = str_of(&v["aiTitle"]),
        Some("session-meta") => {
            if let Some(id) = str_of(&v["sessionId"]) {
                s.id = id;
            }
            // A plain string in the builds seen so far; anything else is left alone.
            if let Some(t) = str_of(&v["meta"]["codebuddy.ai/sessionTitle"]) {
                s.ai_title = Some(t);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_items() {
        let text = [
            r#"{"id":"i1","timestamp":1762596005000,"type":"message","role":"user","content":[{"type":"input_text","text":"list the files here"}],"providerData":{},"cwd":"/home/u/proj"}"#,
            r#"{"id":"i2","timestamp":1762596009000,"type":"message","role":"assistant","content":[{"type":"output_text","text":"There are 3 files."}]}"#,
            r#"{"id":"i3","timestamp":1762596010000,"type":"message","role":"user","content":[{"type":"input_text","text":"<system-reminder>x"}],"providerData":{"isMeta":true}}"#,
            r#"{"id":"i4","timestamp":1762596062000,"type":"message","role":"user","content":[{"type":"input_text","text":"now add a README"}],"providerData":{}}"#,
            r#"{"type":"session-meta","id":"x","sessionId":"S1","timestamp":1762596063000,"meta":{"codebuddy.ai/sessionTitle":"List files"}}"#,
            "",
        ]
        .join("\n");
        let lim = Limits { max_prompt_chars: 4000, max_indexed_chars: 40000 };
        let mut s = Session::default();
        parse(text.as_bytes(), &mut s, &lim).unwrap();
        assert_eq!(s.id, "S1");
        assert_eq!(s.cwd, "/home/u/proj");
        assert_eq!(s.started_ms, 1762596005000);
        assert_eq!(s.prompts, vec!["list the files here", "now add a README"]);
        assert_eq!(s.title(), "List files");
    }
}
