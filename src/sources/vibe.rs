//! Mistral Vibe: `~/.vibe/logs/session` (`$VIBE_HOME/logs/session`, or
//! `[session_logging].save_dir`). Every session has a `meta.json` - `{"session_id",
//! "parent_session_id", "start_time", "title", "title_source", "git_branch",
//! "environment": {"working_directory"}, "archived_at"}` - rewritten as it goes:
//!
//! * 3.x: `unified/<session id>/meta.json`, the transcript in a content-addressed
//!   store beside it (generations of chunks plus a journal) that is not read here, so
//!   these sessions are found by title and folder;
//! * 2.x: `session_<time>_<id8>/meta.json` with an append-only `messages.jsonl`
//!   (`{"role", "content", "injected"}` lines) whose user lines are the prompts.

use super::common::{jsonl, ms_of, per_folder, read_json, str_of, text_of, Found};
use crate::session::{Limits, Session};
use serde_json::Value;
use std::io::BufReader;
use std::path::{Path, PathBuf};

pub fn list_files(root: &Path) -> Vec<Found> {
    let mut out = per_folder(&root.join("unified"), "meta.json");
    out.extend(per_folder(root, "meta.json"));
    out
}

pub fn companions(meta: &Path) -> Vec<PathBuf> {
    vec![meta.with_file_name("messages.jsonl")]
}

pub fn parse_file(meta: &Path, s: &mut Session, lim: &Limits) -> std::io::Result<()> {
    let m: Value = read_json(meta)?;
    if str_of(&m["archived_at"]).is_some() {
        return Ok(());
    }
    s.id = str_of(&m["session_id"]).unwrap_or_default();
    s.cwd =
        str_of(&m["environment"]["working_directory"]).or_else(|| str_of(&m["origin_directory"])).unwrap_or_default();
    s.started_ms = ms_of(&m["start_time"]).unwrap_or(0);
    s.branch = str_of(&m["git_branch"]);
    let title = str_of(&m["title"]);
    if m["title_source"] == "manual" {
        s.custom_title = title;
    } else {
        s.ai_title = title;
    }
    if str_of(&m["parent_session_id"]).is_some() {
        s.entry = Some("subagent".into());
    }
    if let Ok(f) = std::fs::File::open(meta.with_file_name("messages.jsonl")) {
        jsonl::<Value>(BufReader::new(f), &["\"role\":\"user\"", "\"role\": \"user\""], |v| {
            if v["role"] == "user" && v["injected"] != true {
                s.push_prompt(&text_of(&v["content"]), lim);
            }
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::common::testing::*;

    #[test]
    fn reads_both_layouts() {
        let root = TempDir::new("vibe");
        let new = root.join("unified/b7e1");
        let old = root.join("session_20260301_100000_a1b2c3d4");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(new.join("meta.json"), r#"{"session_id":"b7e1","parent_session_id":null,"start_time":"2026-11-08T10:00:05+00:00","git_branch":"main","environment":{"working_directory":"/home/u/proj"},"title":"List files and add README","title_source":"auto","archived_at":null}"#).unwrap();
        std::fs::write(old.join("meta.json"), r#"{"session_id":"a1b2c3d4-full","start_time":"2026-03-01T10:00:00+00:00","environment":{"working_directory":"/w"}}"#).unwrap();
        std::fs::write(
            old.join("messages.jsonl"),
            "{\"role\":\"user\",\"content\":\"list the files here\",\"injected\":false}\n\
             {\"role\":\"assistant\",\"content\":\"3 files\"}\n\
             {\"role\":\"user\",\"content\":\"context\",\"injected\":true}\n",
        )
        .unwrap();
        let files = list_files(&root);
        assert_eq!(files.len(), 2);
        let lim = Limits::TEST;
        let mut a = Session::default();
        parse_file(&new.join("meta.json"), &mut a, &lim).unwrap();
        assert_eq!((a.id.as_str(), a.cwd.as_str()), ("b7e1", "/home/u/proj"));
        assert_eq!(a.title(), "List files and add README");
        let mut b = Session::default();
        parse_file(&old.join("meta.json"), &mut b, &lim).unwrap();
        assert_eq!(b.prompts, vec!["list the files here"]);
    }
}
