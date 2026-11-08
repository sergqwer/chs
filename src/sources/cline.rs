//! Cline CLI (the TypeScript `cline` built on Cline's SDK): `~/.cline/data`
//! (`$CLINE_DATA_DIR`, `$CLINE_DIR/data`), one folder per session:
//!
//! * `sessions/<id>/<id>.json`, a manifest: `{"session_id", "started_at",
//!   "interactive", "is_subagent", "parent_session_id", "cwd", "workspace_root",
//!   "metadata": {"title"}}`;
//! * `<id>.messages.json` beside it, `{"messages": [{"role", "content": [parts]}]}`,
//!   where a prompt is a user message with a `text` part (tool results come back as
//!   user messages too).
//!
//! `data/db/sessions.db` indexes the same manifests.

use super::common::{ms_of, read_json, stat, str_of, subdirs, text_of, Found};
use crate::session::{Limits, Session};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn list_files(root: &Path) -> Vec<Found> {
    subdirs(&root.join("sessions"))
        .filter_map(|d| {
            let id = d.file_name()?.to_str()?.to_string();
            stat(d.join(format!("{id}.json")))
        })
        .collect()
}

/// The transcript, which changes on every turn while the manifest may not.
pub fn companions(manifest: &Path) -> Vec<PathBuf> {
    vec![messages_path(manifest)]
}

fn messages_path(manifest: &Path) -> PathBuf {
    let stem = manifest.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    manifest.with_file_name(format!("{stem}.messages.json"))
}

#[derive(Deserialize)]
struct Messages {
    #[serde(default)]
    messages: Vec<Message>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    role: String,
    #[serde(default)]
    content: Value,
}

pub fn parse_file(path: &Path, s: &mut Session, lim: &Limits) -> std::io::Result<()> {
    let m: Value = read_json(path)?;
    s.id = str_of(&m["session_id"]).unwrap_or_default();
    s.cwd = str_of(&m["cwd"]).or_else(|| str_of(&m["workspace_root"])).unwrap_or_default();
    s.started_ms = ms_of(&m["started_at"]).unwrap_or(0);
    s.custom_title = str_of(&m["metadata"]["title"]);
    s.entry = if m["is_subagent"] == true || str_of(&m["parent_session_id"]).is_some() {
        Some("subagent".into())
    } else if m["interactive"] == false {
        Some("noninteractive".into())
    } else {
        None
    };
    let msgs = str_of(&m["messages_path"]).map(PathBuf::from).filter(|p| p.is_file());
    let msgs = msgs.unwrap_or_else(|| messages_path(path));
    if let Ok(list) = read_json::<Messages>(&msgs) {
        for msg in list.messages.iter().filter(|m| m.role == "user") {
            s.push_prompt(&text_of(&msg.content), lim);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::common::testing::*;

    #[test]
    fn reads_manifest_and_messages() {
        let root = TempDir::new("cline");
        let dir = root.join("sessions/1784094124598_rruoq");
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = dir.join("1784094124598_rruoq.json");
        std::fs::write(
            &manifest,
            r#"{"version":1,"session_id":"1784094124598_rruoq","source":"cli","started_at":"2026-07-15T05:42:04.609Z",
               "status":"completed","interactive":true,"cwd":"/p","workspace_root":"/p","metadata":{"title":"add flag"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("1784094124598_rruoq.messages.json"),
            r#"{"version":1,"messages":[
                {"id":"m1","role":"user","ts":1783376868634,"content":[{"type":"text","text":"add flag"}]},
                {"id":"m2","role":"assistant","content":[{"type":"text","text":"ok"},{"type":"tool_use","name":"x"}]},
                {"id":"m3","role":"user","content":[{"type":"tool_result","content":"done"}]},
                {"id":"m4","role":"user","content":[{"type":"text","text":"add test"}]}]}"#,
        )
        .unwrap();
        assert_eq!(paths(list_files(&root)), vec![manifest.clone()]);
        let lim = Limits::TEST;
        let mut s = Session::default();
        parse_file(&manifest, &mut s, &lim).unwrap();
        assert_eq!(s.id, "1784094124598_rruoq");
        assert_eq!(s.cwd, "/p");
        assert_eq!(s.prompts, vec!["add flag", "add test"]);
        assert_eq!(s.title(), "add flag");
        assert_eq!(s.entry, None);
    }
}
