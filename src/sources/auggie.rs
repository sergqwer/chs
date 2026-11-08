//! Augment's `auggie`: `~/.augment/sessions/<sessionId>.json`, one JSON file per
//! session rewritten as it goes (`<id>-backupN.json` and `*.tmp` are its safety copies):
//! `{"sessionId", "created", "modified", "title", "customTitle", "parentConversationId",
//! "chatHistory": [{"exchange": {"request_message", "request_nodes"}}]}`. The folder
//! is in a request node of type 4: `ide_state_node.workspace_folders[0]`
//! `.repository_root` / `.folder_root`.

use super::common::{files, ms_of, read_json, Found};
use crate::session::{Limits, Session};
use serde::Deserialize;
use std::path::Path;

pub fn list_files(root: &Path) -> Vec<Found> {
    files(&root.join("sessions"), &|n| n.ends_with(".json") && !is_backup(n))
}

fn is_backup(name: &str) -> bool {
    let stem = name.trim_end_matches(".json");
    stem.rsplit_once("-backup").is_some_and(|(_, n)| n.chars().all(|c| c.is_ascii_digit()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    #[serde(default)]
    session_id: String,
    created: Option<serde_json::Value>,
    title: Option<String>,
    custom_title: Option<String>,
    parent_conversation_id: Option<String>,
    #[serde(default)]
    chat_history: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    exchange: Option<Exchange>,
}

#[derive(Deserialize)]
struct Exchange {
    request_message: Option<String>,
    #[serde(default)]
    request_nodes: Vec<Node>,
}

#[derive(Deserialize)]
struct Node {
    #[serde(rename = "type")]
    ty: Option<i64>,
    ide_state_node: Option<IdeState>,
}

#[derive(Deserialize)]
struct IdeState {
    #[serde(default)]
    workspace_folders: Vec<Folder>,
}

#[derive(Deserialize)]
struct Folder {
    repository_root: Option<String>,
    folder_root: Option<String>,
}

pub fn parse_file(path: &Path, s: &mut Session, lim: &Limits) -> std::io::Result<()> {
    let f: File = read_json(path)?;
    s.id = f.session_id;
    s.started_ms = f.created.as_ref().and_then(ms_of).unwrap_or(0);
    s.custom_title = f.custom_title.filter(|t| !t.trim().is_empty());
    s.ai_title = f.title.filter(|t| !t.trim().is_empty());
    if f.parent_conversation_id.is_some_and(|p| !p.is_empty()) {
        s.entry = Some("subagent".into());
    }
    for ex in f.chat_history.into_iter().filter_map(|i| i.exchange) {
        if s.cwd.is_empty() {
            let folder = ex
                .request_nodes
                .iter()
                .filter(|n| n.ty == Some(4))
                .filter_map(|n| n.ide_state_node.as_ref()?.workspace_folders.first())
                .find_map(|f| f.repository_root.clone().or_else(|| f.folder_root.clone()));
            s.cwd = folder.unwrap_or_default();
        }
        if let Some(m) = ex.request_message {
            s.push_prompt(&m, lim);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::common::testing::*;

    #[test]
    fn reads_session_file() {
        assert!(is_backup("abc-backup2.json"));
        assert!(!is_backup("abc.json"));
        let dir = TempDir::new("aug");
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        let f = dir.join("sessions/s1.json");
        std::fs::write(
            &f,
            r#"{"sessionId":"s1","created":"2026-11-08T10:00:00.000Z","modified":"2026-11-08T11:00:00.000Z","title":"Add flag",
              "chatHistory":[
                {"exchange":{"request_message":"add a flag","response_text":"ok","request_nodes":[
                   {"id":1,"type":4,"ide_state_node":{"workspace_folders":[{"repository_root":"/home/u/p","folder_root":"/home/u/p/sub"}]}}]}},
                {"exchange":{"request_message":"and a test","response_text":"done","request_nodes":[]}}]}"#,
        )
        .unwrap();
        std::fs::write(dir.join("sessions/s1-backup1.json"), "{}").unwrap();
        assert_eq!(paths(list_files(&dir)), vec![f.clone()]);
        let lim = Limits::TEST;
        let mut s = Session::default();
        parse_file(&f, &mut s, &lim).unwrap();
        assert_eq!(s.id, "s1");
        assert_eq!(s.cwd, "/home/u/p");
        assert_eq!(s.prompts, vec!["add a flag", "and a test"]);
        assert_eq!(s.title(), "Add flag");
    }
}
