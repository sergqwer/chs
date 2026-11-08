//! GitHub Copilot CLI: `$COPILOT_HOME` or `~/.copilot`, one folder per session,
//! `session-state/<uuid>/events.jsonl` (append-only; `workspace.yaml` beside it is a
//! rewritten summary of the same data). Each line is `{"type", "timestamp", "data",
//! "agentId"?}`:
//!
//! * `session.start` / `session.resume` - `data.sessionId`, `data.context.{cwd, branch}`;
//! * `session.title_changed` - `data.title`;
//! * `user.message` - `data.content`. Only events without `agentId` (subagents) and
//!   with no `data.source` or `"user"` are typed by a person; `skill-*`, `agent-*` and
//!   autopilot continuations are injected - the same filter Copilot's own history uses.

use super::common::{jsonl, ms_of, per_folder, str_of, Found};
use crate::session::{Limits, Session};
use serde_json::Value;
use std::io::BufRead;
use std::path::Path;

pub fn list_files(root: &Path) -> Vec<Found> {
    per_folder(&root.join("session-state"), "events.jsonl")
}

const NEEDLES: [&str; 4] = ["\"session.start\"", "\"session.resume\"", "\"session.title_changed\"", "\"user.message\""];

pub fn parse(reader: impl BufRead, s: &mut Session, lim: &Limits) -> std::io::Result<u64> {
    jsonl::<Value>(reader, &NEEDLES, |v| apply(&v, s, lim))
}

fn apply(v: &Value, s: &mut Session, lim: &Limits) {
    let d = &v["data"];
    match v["type"].as_str() {
        Some(t @ ("session.start" | "session.resume")) => {
            if t == "session.start" {
                if let Some(id) = str_of(&d["sessionId"]) {
                    s.id = id;
                }
                s.started_ms = ms_of(&d["startTime"]).or_else(|| ms_of(&v["timestamp"])).unwrap_or(0);
            }
            let ctx = &d["context"];
            if s.cwd.is_empty() {
                s.cwd = str_of(&ctx["cwd"]).unwrap_or_default();
            }
            if let Some(b) = str_of(&ctx["branch"]).filter(|b| b != "HEAD") {
                s.branch = Some(b);
            }
        }
        Some("session.title_changed") => {
            if let Some(t) = str_of(&d["title"]) {
                s.ai_title = Some(t);
            }
        }
        Some("user.message") => {
            let typed = v.get("agentId").is_none_or(Value::is_null)
                && d.get("source").and_then(Value::as_str).is_none_or(|x| x == "user")
                && d.get("isAutopilotContinuation").and_then(Value::as_bool) != Some(true);
            if typed {
                if let Some(text) = d["content"].as_str() {
                    s.push_prompt(text, lim);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_events() {
        let text = [
            r#"{"id":"e1","timestamp":"2026-11-08T10:00:00.000Z","parentId":null,"type":"session.start","data":{"sessionId":"7f3c","startTime":"2026-11-08T10:00:00.000Z","context":{"cwd":"/home/u/proj","branch":"main"}}}"#,
            r#"{"id":"e2","timestamp":"2026-11-08T10:00:05.000Z","type":"user.message","data":{"content":"fix the failing test","source":"user"}}"#,
            r#"{"id":"e3","timestamp":"2026-11-08T10:00:20.000Z","type":"assistant.message","data":{"content":"Done."}}"#,
            r#"{"id":"e4","timestamp":"2026-11-08T10:00:30.000Z","type":"user.message","agentId":"sub1","data":{"content":"subagent task"}}"#,
            r#"{"id":"e5","timestamp":"2026-11-08T10:00:40.000Z","type":"user.message","data":{"content":"skill text","source":"skill-pdf"}}"#,
            r#"{"id":"e6","timestamp":"2026-11-08T10:01:00.000Z","type":"session.title_changed","data":{"title":"Fix failing test"}}"#,
            r#"{"id":"e7","timestamp":"2026-11-08T10:02:00.000Z","type":"user.message","data":{"content":"now add a changelog entry"}}"#,
            "",
        ]
        .join("\n");
        let lim = Limits::TEST;
        let mut s = Session::default();
        parse(text.as_bytes(), &mut s, &lim).unwrap();
        assert_eq!(s.id, "7f3c");
        assert_eq!(s.cwd, "/home/u/proj");
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.prompts, vec!["fix the failing test", "now add a changelog entry"]);
        assert_eq!(s.title(), "Fix failing test");
    }
}
