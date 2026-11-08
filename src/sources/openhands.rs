//! OpenHands CLI (OpenHands-CLI on the software-agent-sdk): `~/.openhands`
//! (`$OPENHANDS_PERSISTENCE_DIR`), one folder per conversation,
//! `conversations/<id>/base_state.json` (`{"id", "workspace": {"working_dir"}}`) and
//! `events/event-<n>-<uuid>.json`, one small JSON file per event. A prompt is an event
//! with `kind == "MessageEvent"` and `source == "user"`, its text in
//! `llm_message.content`; context the CLI attaches goes to `extended_content` and is
//! not part of it. There is no title: like the CLI itself, the first prompt is used.

use super::common::{ms_of, per_folder, read_json, str_of, text_of, Found};
use crate::session::{Limits, Session};
use memchr::memmem;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn list_files(root: &Path) -> Vec<Found> {
    // `$OPENHANDS_CONVERSATIONS_DIR` points at the conversations folder itself.
    let conv = root.join("conversations");
    per_folder(if conv.is_dir() { &conv } else { root }, "base_state.json")
}

/// A new event is a new file, which changes the folder's mtime; the newest event
/// file itself covers one still being written when the index last looked.
pub fn companions(state: &Path) -> Vec<PathBuf> {
    let dir = state.with_file_name("events");
    let mut out: Vec<PathBuf> = events(&dir).pop().into_iter().collect();
    out.push(dir);
    out
}

/// `event-<n>-<uuid>.json` files in event order (by `n`, not by name: the padding
/// is 5 digits and runs out past 99999).
fn events(dir: &Path) -> Vec<PathBuf> {
    let mut list: Vec<(u64, PathBuf)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let name = e.file_name().into_string().ok()?;
                    let n = name.strip_prefix("event-")?.split('-').next()?.parse().ok()?;
                    name.ends_with(".json").then(|| (n, e.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    list.sort();
    list.into_iter().map(|(_, p)| p).collect()
}

pub fn parse_file(path: &Path, s: &mut Session, lim: &Limits) -> std::io::Result<()> {
    let state: Value = read_json(path)?;
    // `--resume` takes the folder name (the id without dashes).
    s.id = path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or_default().to_string();
    if s.id.is_empty() {
        s.id = str_of(&state["id"]).unwrap_or_default();
    }
    s.cwd = str_of(&state["workspace"]["working_dir"]).unwrap_or_default();

    let events = events(&path.with_file_name("events"));
    let kind = memmem::Finder::new(b"\"MessageEvent\"");
    for (i, e) in events.iter().enumerate() {
        let Ok(bytes) = std::fs::read(e) else { continue };
        if i == 0 {
            s.started_ms =
                serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| ms_of(&v["timestamp"])).unwrap_or(0);
        }
        if kind.find(&bytes).is_none() {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&bytes) else { continue };
        if v["kind"] == "MessageEvent" && v["source"] == "user" {
            s.push_prompt(&text_of(&v["llm_message"]["content"]), lim);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::common::testing::*;

    #[test]
    fn reads_conversation() {
        let root = TempDir::new("oh");
        let conv = root.join("conversations/3f2a00aa");
        std::fs::create_dir_all(conv.join("events")).unwrap();
        let state = conv.join("base_state.json");
        std::fs::write(&state, r#"{"id":"3f2a00aa","workspace":{"kind":"LocalWorkspace","working_dir":"/home/u/p"}}"#)
            .unwrap();
        let ev =
            |n: u32, body: &str| std::fs::write(conv.join(format!("events/event-{n:05}-x{n}.json")), body).unwrap();
        ev(0, r#"{"kind":"SystemPromptEvent","timestamp":"2026-11-08T10:00:00.1","source":"agent"}"#);
        ev(
            1,
            r#"{"kind":"MessageEvent","timestamp":"2026-11-08T10:00:01","source":"user","llm_message":{"role":"user","content":[{"type":"text","text":"add flag"}]},"extended_content":[{"type":"text","text":"ctx"}]}"#,
        );
        ev(
            2,
            r#"{"kind":"MessageEvent","timestamp":"2026-11-08T10:00:02","source":"agent","llm_message":{"role":"assistant","content":[{"type":"text","text":"ok"}]}}"#,
        );
        ev(
            3,
            r#"{"kind":"MessageEvent","timestamp":"2026-11-08T10:00:03","source":"user","llm_message":{"role":"user","content":[{"type":"text","text":"add test"}]}}"#,
        );
        // Past the 5-digit padding: still after event 3.
        let late = conv.join("events/event-100000-z.json");
        std::fs::write(&late, r#"{"kind":"MessageEvent","source":"user","llm_message":{"content":[{"type":"text","text":"much later"}]}}"#).unwrap();
        assert_eq!(companions(&state)[0], late);
        assert_eq!(paths(list_files(&root)), vec![state.clone()]);
        let lim = Limits::TEST;
        let mut s = Session::default();
        parse_file(&state, &mut s, &lim).unwrap();
        assert_eq!(s.id, "3f2a00aa");
        assert_eq!(s.cwd, "/home/u/p");
        assert_eq!(s.prompts, vec!["add flag", "add test", "much later"]);
        assert!(s.started_ms > 0);
    }
}
