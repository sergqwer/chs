//! Cursor CLI (`agent`, formerly `cursor-agent`): `~/.cursor` (`$CURSOR_CONFIG_DIR`).
//!
//! * `chats/<md5 of cwd>/<chat id>/meta.json` - what the CLI's own session list reads:
//!   `{"createdAtMs", "updatedAtMs", "hasConversation", "title", "isSubagent", "cwd"}`.
//!   Beside it `store.db` holds the conversation as protobuf blobs;
//! * `projects/<slug of cwd>/agent-transcripts/<chat id>/<chat id>.jsonl` (older:
//!   `agent-transcripts/<chat id>.jsonl`) - a plain transcript the CLI appends as it
//!   goes, `{"role": "user", "message": {"content": [{"type": "text", "text"}]}}`,
//!   the prompt wrapped in `<user_query>`. The slug is the path with every run of
//!   characters other than ASCII letters and digits turned into one `-`, trimmed.
//!
//! Prompts come from the transcript; the list, folder and title from meta.json.

use super::common::{jsonl, per_folder, read_json, str_of, subdirs, text_of, Found};
use crate::session::{Limits, Session};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn list_files(root: &Path) -> Vec<Found> {
    subdirs(&root.join("chats")).flat_map(|workspace| per_folder(&workspace, "meta.json")).collect()
}

/// The transcript; finding it takes the folder from meta.json.
pub fn companions(meta: &Path) -> Vec<PathBuf> {
    transcript(meta).into_iter().collect()
}

fn transcript(meta: &Path) -> Option<PathBuf> {
    let chat = meta.parent()?;
    let id = chat.file_name()?.to_str()?;
    let root = chat.parent()?.parent()?.parent()?;
    let v: Value = read_json(meta).ok()?;
    let dir = root.join("projects").join(slug(v["cwd"].as_str()?)).join("agent-transcripts");
    [dir.join(id).join(format!("{id}.jsonl")), dir.join(format!("{id}.jsonl"))].into_iter().find(|p| p.is_file())
}

fn slug(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// `transcript` is the companion found for this chat, if any.
pub fn parse_file(meta: &Path, transcript: Option<&Path>, s: &mut Session, lim: &Limits) -> std::io::Result<()> {
    let v: Value = read_json(meta)?;
    if v["hasConversation"] == false {
        return Ok(());
    }
    s.id = meta.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or_default().to_string();
    s.cwd = str_of(&v["cwd"]).unwrap_or_default();
    s.started_ms = v["createdAtMs"].as_i64().unwrap_or(0);
    s.custom_title = str_of(&v["title"]).filter(|t| t != "New Agent" && t != "New Chat");
    if v["isSubagent"] == true {
        s.entry = Some("subagent".into());
    }
    if let Some(t) = transcript {
        let file = std::fs::File::open(t)?;
        jsonl::<Value>(std::io::BufReader::new(file), &["\"role\":\"user\"", "\"role\": \"user\""], |line| {
            if line["role"] == "user" {
                s.push_prompt(&user_query(&text_of(&line["message"]["content"])), lim);
            }
        })?;
    }
    Ok(())
}

/// The text inside `<user_query>`, without the context blocks around it.
fn user_query(text: &str) -> String {
    match (text.find("<user_query>"), text.rfind("</user_query>")) {
        (Some(a), Some(b)) if a + 12 <= b => text[a + 12..b].trim().to_string(),
        _ => text.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::common::testing::*;

    #[test]
    fn slugs_like_cursor() {
        assert_eq!(slug("/home/u/my proj"), "home-u-my-proj");
        assert_eq!(slug("C:\\Users\\u\\app"), "C-Users-u-app");
        assert_eq!(slug("/x//y/"), "x-y");
    }

    #[test]
    fn reads_meta_and_transcript() {
        let root = TempDir::new("cursor");
        let chat = root.join("chats/0123abcd/0c9a");
        std::fs::create_dir_all(&chat).unwrap();
        let meta = chat.join("meta.json");
        std::fs::write(&meta, r#"{"schemaVersion":1,"createdAtMs":1794300000000,"updatedAtMs":1794300120000,"hasConversation":true,"title":"Fix failing test","cwd":"/home/u/proj"}"#).unwrap();
        let tdir = root.join("projects/home-u-proj/agent-transcripts/0c9a");
        std::fs::create_dir_all(&tdir).unwrap();
        std::fs::write(
            tdir.join("0c9a.jsonl"),
            concat!(
                r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_info>os</user_info>\n<user_query>\nfix the failing test\n</user_query>"}]}}"#,
                "\n",
                r#"{"role":"assistant","message":{"content":[{"type":"text","text":"Done."},{"type":"tool_use","name":"Shell","input":{}}]}}"#,
                "\n",
                r#"{"role":"user","message":{"content":[{"type":"text","text":"now add a changelog entry"}]}}"#,
                "\n"
            ),
        )
        .unwrap();
        assert_eq!(paths(list_files(&root)), vec![meta.clone()]);
        assert_eq!(companions(&meta), vec![tdir.join("0c9a.jsonl")]);
        let lim = Limits::TEST;
        let mut s = Session::default();
        parse_file(&meta, companions(&meta).first().map(PathBuf::as_path), &mut s, &lim).unwrap();
        assert_eq!(s.id, "0c9a");
        assert_eq!(s.cwd, "/home/u/proj");
        assert_eq!(s.prompts, vec!["fix the failing test", "now add a changelog entry"]);
        assert_eq!(s.title(), "Fix failing test");
    }
}
