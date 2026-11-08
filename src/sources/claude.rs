//! Claude Code (and anything run with a relocated CLAUDE_CONFIG_DIR, like claudellm):
//! `<root>/projects/<encoded-cwd>/<session-id>.jsonl`, one JSON object per line.
//!
//! Only the few line kinds that matter are deserialized; a cheap substring test skips
//! the (often multi-megabyte) assistant and tool-result lines without parsing them.

use super::common::{files_in_subdirs, jsonl, Found};
use crate::session::{parse_ts, Limits, Session};
use serde::de::{self, Deserializer, IgnoredAny, SeqAccess, Visitor};
use serde::Deserialize;
use std::fmt;
use std::io::BufRead;
use std::path::Path;

/// Session files of one Claude config root. Subagent transcripts live one level
/// deeper (`<session>/subagents/*.jsonl`) and are not conversations of their own.
pub fn list_files(root: &Path) -> Vec<Found> {
    files_in_subdirs(&root.join("projects"), &|n| n.ends_with(".jsonl"))
}

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    ty: String,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    timestamp: Option<String>,
    entrypoint: Option<String>,
    #[serde(rename = "isMeta", default)]
    is_meta: bool,
    #[serde(rename = "isSidechain", default)]
    is_sidechain: bool,
    #[serde(rename = "gitBranch")]
    git_branch: Option<String>,
    message: Option<Message>,
    #[serde(rename = "customTitle")]
    custom_title: Option<String>,
    #[serde(rename = "aiTitle")]
    ai_title: Option<String>,
    summary: Option<String>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Content,
}

/// `message.content` is either a plain string or an array of typed parts. Only text
/// parts are kept; tool results (which can be huge) are skipped without allocating.
#[derive(Default)]
struct Content(Vec<String>);

impl<'de> Deserialize<'de> for Content {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Content;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string or an array of content parts")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Content, E> {
                Ok(Content(vec![v.to_string()]))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Content, E> {
                Ok(Content(vec![v]))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Content, E> {
                Ok(Content::default())
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Content, A::Error> {
                let mut out = Vec::new();
                while let Some(part) = seq.next_element::<Part>()? {
                    if part.ty.as_deref() == Some("text") {
                        if let Some(t) = part.text {
                            out.push(t);
                        }
                    }
                }
                Ok(Content(out))
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Content, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Content::default())
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Deserialize)]
struct Part {
    #[serde(rename = "type")]
    ty: Option<String>,
    text: Option<String>,
}

// Claude Code writes compact JSON; the spaced spellings keep other writers working.
const NEEDLES: [&str; 8] = [
    "\"type\":\"user\"",
    "\"type\":\"custom-title\"",
    "\"type\":\"ai-title\"",
    "\"type\":\"summary\"",
    "\"type\": \"user\"",
    "\"type\": \"custom-title\"",
    "\"type\": \"ai-title\"",
    "\"type\": \"summary\"",
];

/// Parse complete lines from `reader` into `s` (which may already hold the result of
/// an earlier, shorter read of the same file). Returns the number of bytes consumed;
/// a trailing line without its newline is still being written and is left for later.
pub fn parse(reader: impl BufRead, s: &mut Session, lim: &Limits) -> std::io::Result<u64> {
    jsonl::<Line>(reader, &NEEDLES, |line| apply(line, s, lim))
}

fn apply(line: Line, s: &mut Session, lim: &Limits) {
    match line.ty.as_str() {
        "custom-title" => {
            if let Some(t) = line.custom_title.filter(|t| !t.trim().is_empty()) {
                s.custom_title = Some(t);
            }
        }
        "ai-title" => {
            if let Some(t) = line.ai_title.filter(|t| !t.trim().is_empty()) {
                s.ai_title = Some(t);
            }
        }
        "summary" => {
            if let Some(t) = line.summary.filter(|t| !t.trim().is_empty()) {
                s.summary = Some(t);
            }
        }
        "user" => {
            if line.is_sidechain {
                return;
            }
            if s.id.is_empty() {
                if let Some(id) = &line.session_id {
                    s.id = id.clone();
                }
            }
            if s.cwd.is_empty() {
                if let Some(c) = &line.cwd {
                    s.cwd = c.clone();
                }
            }
            if s.started_ms == 0 {
                if let Some(ms) = line.timestamp.as_deref().and_then(parse_ts) {
                    s.started_ms = ms;
                }
            }
            if s.entry.is_none() {
                s.entry = line.entrypoint.clone();
            }
            if let Some(b) = line.git_branch.filter(|b| !b.is_empty() && b != "HEAD") {
                s.branch = Some(b);
            }
            if line.is_meta {
                return;
            }
            let Some(msg) = line.message else { return };
            for text in msg.content.0 {
                if let Some(p) = clean_prompt(&text) {
                    s.push_prompt(&p, lim);
                }
            }
        }
        _ => {}
    }
}

/// Drop what Claude Code records as "user" but the user did not type: slash-command
/// envelopes, local command output, hook/system injections, interruption markers.
fn clean_prompt(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(rest) = t.strip_prefix("<bash-input>") {
        let cmd = rest.split("</bash-input>").next().unwrap_or("").trim();
        return (!cmd.is_empty()).then(|| format!("!{cmd}"));
    }
    const SKIP: [&str; 13] = [
        "<command-name>",
        "<command-message>",
        "<command-args>",
        "<local-command-stdout>",
        "<local-command-stderr>",
        "<local-command-caveat>",
        "<bash-stdout>",
        "<bash-stderr>",
        "<task-notification>",
        "<system-reminder>",
        "<user-prompt-submit-hook>",
        "[Request interrupted",
        "Caveat: The messages below",
    ];
    if SKIP.iter().any(|p| t.starts_with(p)) {
        return None;
    }
    Some(t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIM: Limits = Limits::TEST;

    fn run(lines: &[&str]) -> (Session, u64) {
        let data = lines.iter().map(|l| format!("{l}\n")).collect::<String>();
        let mut s = Session::default();
        let n = parse(data.as_bytes(), &mut s, &LIM).unwrap();
        (s, n)
    }

    #[test]
    fn parses_a_session() {
        let (s, _) = run(&[
            r#"{"type":"permission-mode","permissionMode":"default","sessionId":"abc"}"#,
            r#"{"parentUuid":null,"isSidechain":false,"type":"user","message":{"role":"user","content":"<command-name>/model</command-name>"},"cwd":"/home/u/proj","sessionId":"abc","timestamp":"2026-11-01T10:00:00.000Z","entrypoint":"cli","gitBranch":"main"}"#,
            r#"{"isSidechain":false,"type":"user","isMeta":true,"message":{"role":"user","content":"Caveat: The messages below were generated"},"cwd":"/home/u/proj","sessionId":"abc"}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"Fix the proxy checker please"},"cwd":"/home/u/proj","sessionId":"abc","timestamp":"2026-11-01T10:01:00.000Z"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Sure"}]}}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":[{"tool_use_id":"t1","type":"tool_result","content":"huge output"}]},"sessionId":"abc"}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":[{"type":"text","text":"now add tests"},{"type":"image","source":{}}]},"sessionId":"abc"}"#,
            r#"{"type":"ai-title","aiTitle":"Proxy checker fix","sessionId":"abc"}"#,
        ]);
        assert_eq!(s.id, "abc");
        assert_eq!(s.cwd, "/home/u/proj");
        assert_eq!(s.entry.as_deref(), Some("cli"));
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.prompts, vec!["Fix the proxy checker please", "now add tests"]);
        assert_eq!(s.title(), "Proxy checker fix");
        assert!(s.started_ms > 0);
    }

    #[test]
    fn custom_title_wins_and_sidechain_ignored() {
        let (s, _) = run(&[
            r#"{"type":"user","isSidechain":true,"message":{"content":"agent brief"},"sessionId":"x","cwd":"/a"}"#,
            r#"{"type":"user","message":{"content":"real prompt"},"sessionId":"x","cwd":"/a"}"#,
            r#"{"type":"ai-title","aiTitle":"Auto","sessionId":"x"}"#,
            r#"{"type":"custom-title","customTitle":"my-name","sessionId":"x"}"#,
        ]);
        assert_eq!(s.prompts, vec!["real prompt"]);
        assert_eq!(s.title(), "my-name");
    }

    #[test]
    fn incomplete_last_line_is_left_for_later() {
        let data =
            "{\"type\":\"user\",\"message\":{\"content\":\"one\"},\"sessionId\":\"x\"}\n{\"type\":\"user\",\"mess";
        let mut s = Session::default();
        let n = parse(data.as_bytes(), &mut s, &LIM).unwrap();
        assert_eq!(n as usize, data.find('\n').unwrap() + 1);
        assert_eq!(s.prompts, vec!["one"]);
    }

    #[test]
    fn bash_input_becomes_bang_command() {
        assert_eq!(clean_prompt("<bash-input>ls -la</bash-input>").as_deref(), Some("!ls -la"));
        assert_eq!(clean_prompt("<local-command-stdout>x</local-command-stdout>"), None);
    }
}
