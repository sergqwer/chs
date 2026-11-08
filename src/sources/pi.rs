//! Pi (earendil-works/pi, formerly badlogic/pi-mono): `<agent dir>/sessions/--<cwd>--/
//! <timestamp>_<uuid>.jsonl`, the agent dir being `$PI_CODING_AGENT_DIR` or
//! `~/.pi/agent`. Append-only JSONL:
//!
//! * the first line is the header `{"type":"session","id","timestamp","cwd"}`;
//! * `{"type":"message","message":{"role":"user","content":...}}` is a prompt, the
//!   content a string or an array of `{"type":"text"}` / image parts;
//! * `{"type":"session_info","name"}` is the name given with `/name` or `--name`.
//!
//! Entries form a tree (branches live in the same file); every user message counts.

use super::common::{jsonl, ms_of, str_of, text_of, walk};
use crate::session::{Limits, Session};
use serde_json::Value;
use std::io::BufRead;
use std::path::{Path, PathBuf};

pub fn list_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    // `--session-dir` folders are flat; the default one has a folder per cwd.
    walk(&root.join("sessions"), 1, &|n| n.ends_with(".jsonl"), &mut out);
    out
}

const NEEDLES: [&str; 4] = ["\"session\"", "\"session_info\"", "\"role\":\"user\"", "\"role\": \"user\""];

pub fn parse(reader: impl BufRead, s: &mut Session, lim: &Limits) -> std::io::Result<u64> {
    jsonl::<Value>(reader, &NEEDLES, |v| apply(&v, s, lim))
}

fn apply(v: &Value, s: &mut Session, lim: &Limits) {
    match v["type"].as_str() {
        Some("session") => {
            if let Some(id) = str_of(&v["id"]) {
                s.id = id;
            }
            if let Some(cwd) = str_of(&v["cwd"]) {
                s.cwd = cwd;
            }
            if let Some(ms) = ms_of(&v["timestamp"]) {
                s.started_ms = ms;
            }
        }
        Some("session_info") => s.custom_title = str_of(&v["name"]),
        Some("message") if v["message"]["role"] == "user" => {
            if s.started_ms == 0 {
                s.started_ms = ms_of(&v["timestamp"]).unwrap_or(0);
            }
            s.push_prompt(&text_of(&v["message"]["content"]), lim);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_header_prompts_and_name() {
        let text = [
            r#"{"type":"session","version":3,"id":"0b6f-1","timestamp":"2026-11-08T10:00:00.000Z","cwd":"/home/u/p"}"#,
            r#"{"type":"message","id":"a1","parentId":null,"timestamp":"2026-11-08T10:00:01.000Z","message":{"role":"system","content":"","sections":{"preamble":"You are"}}}"#,
            r#"{"type":"message","id":"a2","parentId":"a1","timestamp":"2026-11-08T10:00:02.000Z","message":{"role":"user","content":"add flag","timestamp":1}}"#,
            r#"{"type":"message","id":"a3","parentId":"a2","timestamp":"2026-11-08T10:00:03.000Z","message":{"role":"assistant","content":[{"type":"text","text":"the user asked"}]}}"#,
            r#"{"type":"message","id":"a4","parentId":"a3","timestamp":"2026-11-08T10:00:04.000Z","message":{"role":"user","content":[{"type":"text","text":"add test"},{"type":"image","data":"..."}]}}"#,
            r#"{"type":"session_info","id":"a5","parentId":"a4","timestamp":"2026-11-08T10:00:05.000Z","name":"Flags"}"#,
            "",
        ]
        .join("\n");
        let lim = Limits { max_prompt_chars: 4000, max_indexed_chars: 40000 };
        let mut s = Session::default();
        let n = parse(text.as_bytes(), &mut s, &lim).unwrap();
        assert_eq!(n as usize, text.len());
        assert_eq!(s.id, "0b6f-1");
        assert_eq!(s.cwd, "/home/u/p");
        assert_eq!(s.prompts, vec!["add flag", "add test"]);
        assert_eq!(s.title(), "Flags");
    }
}
