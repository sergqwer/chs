//! Continue's `cn` (shared with the Continue IDE extensions):
//! `$CONTINUE_GLOBAL_DIR` or `~/.continue`, `sessions/<sessionId>.json` - one JSON file
//! per session, rewritten on every turn (`sessions.json` beside them is an index):
//! `{"sessionId", "title", "workspaceDirectory", "history": [{"message": {"role",
//! "content"}}]}`. The title stays "Untitled Session" until one is generated, and the
//! workspace may be a `file://` URI when the IDE wrote it.

use super::common::{read_json, str_of, text_of, uri_to_path, walk};
use crate::session::{Limits, Session};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn list_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(&root.join("sessions"), 0, &|n| n.ends_with(".json") && n != "sessions.json", &mut out);
    out
}

pub fn parse_file(path: &Path, s: &mut Session, lim: &Limits) -> std::io::Result<()> {
    let v: Value = read_json(path)?;
    s.id = str_of(&v["sessionId"]).unwrap_or_default();
    s.custom_title = str_of(&v["title"]).filter(|t| t != "Untitled Session");
    s.cwd = str_of(&v["workspaceDirectory"]).map(|w| uri_to_path(&w)).unwrap_or_default();
    for item in v["history"].as_array().into_iter().flatten() {
        let m = &item["message"];
        if m["role"] == "user" {
            s.push_prompt(&text_of(&m["content"]), lim);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_session_file() {
        let dir = std::env::temp_dir().join(format!("chs-cn-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        let f = dir.join("sessions/7c1e.json");
        std::fs::write(
            &f,
            r#"{"sessionId":"7c1e","title":"Untitled Session","workspaceDirectory":"file:///home/u/my%20proj","history":[
             {"message":{"role":"user","content":"add a --verbose flag"},"contextItems":[]},
             {"message":{"role":"assistant","content":"Done."},"contextItems":[]},
             {"message":{"role":"user","content":[{"type":"text","text":"now add a test"}]},"contextItems":[]}]}"#,
        )
        .unwrap();
        std::fs::write(dir.join("sessions/sessions.json"), "[]").unwrap();
        assert_eq!(list_files(&dir), vec![f.clone()]);
        let lim = Limits { max_prompt_chars: 4000, max_indexed_chars: 40000 };
        let mut s = Session::default();
        parse_file(&f, &mut s, &lim).unwrap();
        assert_eq!(s.id, "7c1e");
        assert_eq!(s.cwd, "/home/u/my proj");
        assert_eq!(s.custom_title, None);
        assert_eq!(s.prompts, vec!["add a --verbose flag", "now add a test"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
