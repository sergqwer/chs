//! Moonshot's Kimi, in both generations (the session folder layouts differ, the idea
//! is the same: a rewritten `state.json` plus an append-only `wire.jsonl` of events).
//!
//! * **Kimi Code** (`kimi-code`, current): `~/.kimi-code` (`$KIMI_CODE_HOME`),
//!   `sessions/<workspace id>/<session id>/state.json` - `{"id", "title",
//!   "isCustomTitle", "createdAt", "cwd", "archived"}` - and `agents/main/wire.jsonl`,
//!   where a prompt is `{"type": "turn.prompt", "input": [parts], "origin": {"kind":
//!   "user"}}` (skills, plugins, cron, hooks... are other origins).
//! * **Kimi CLI** (the archived Python `kimi-cli`): `~/.kimi` (`$KIMI_SHARE_DIR`),
//!   `sessions/<md5 of the work dir>/<session id>/wire.jsonl` with prompts as
//!   `{"message": {"type": "TurnBegin", "payload": {"user_input"}}}` and the title in
//!   `state.json` (`custom_title`). The folder is found through `kimi.json`, whose
//!   `work_dirs[].path` hash to the folder names.

use super::common::{jsonl, ms_of, read_json, stat, str_of, subdirs, text_of, Found};
use crate::session::{Limits, Session};
use md5::{Digest, Md5};
use serde_json::Value;
use std::io::BufReader;
use std::path::{Path, PathBuf};

/// `sessions/<workspace>/<session>/<name>` (both generations).
fn session_files(root: &Path, name: &str) -> Vec<Found> {
    subdirs(&root.join("sessions")).flat_map(|ws| subdirs(&ws)).filter_map(|d| stat(d.join(name))).collect()
}

pub fn list_files(root: &Path) -> Vec<Found> {
    session_files(root, "state.json")
}

pub fn companions(state: &Path) -> Vec<PathBuf> {
    vec![state.with_file_name("agents").join("main").join("wire.jsonl")]
}

pub fn parse_file(state: &Path, s: &mut Session, lim: &Limits) -> std::io::Result<()> {
    let st: Value = read_json(state)?;
    if st["archived"] == true {
        return Ok(());
    }
    s.id = str_of(&st["id"]).unwrap_or_default();
    s.cwd = str_of(&st["cwd"]).unwrap_or_default();
    s.started_ms = ms_of(&st["createdAt"]).unwrap_or(0);
    // A "replaceable" title is just the first prompt, so it does no harm as the title.
    let title = str_of(&st["title"]);
    if st["isCustomTitle"] == true {
        s.custom_title = title;
    } else {
        s.ai_title = title;
    }
    let wire = companions(state).remove(0);
    if let Ok(f) = std::fs::File::open(wire) {
        jsonl::<Value>(BufReader::new(f), &["\"turn.prompt\""], |v| {
            if v["type"] == "turn.prompt" && v["origin"]["kind"].as_str().is_none_or(|k| k == "user") {
                s.push_prompt(&text_of(&v["input"]), lim);
            }
        })?;
    }
    Ok(())
}

pub fn legacy_list_files(root: &Path) -> Vec<Found> {
    session_files(root, "wire.jsonl")
}

/// The state, and `kimi.json` at the root, which maps the folder hashes to paths.
pub fn legacy_companions(wire: &Path) -> Vec<PathBuf> {
    let mut out = vec![wire.with_file_name("state.json")];
    if let Some(root) = wire.parent().and_then(Path::parent).and_then(Path::parent).and_then(Path::parent) {
        out.push(root.join("kimi.json"));
    }
    out
}

pub fn legacy_parse_file(wire: &Path, s: &mut Session, lim: &Limits) -> std::io::Result<()> {
    let dir = wire.parent().unwrap_or(Path::new("."));
    s.id = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
    if let Ok(st) = read_json::<Value>(&dir.join("state.json")) {
        if st["archived"] == true {
            s.id.clear();
            return Ok(());
        }
        s.custom_title = str_of(&st["custom_title"]);
    }
    let f = std::fs::File::open(wire)?;
    jsonl::<Value>(BufReader::new(f), &["\"TurnBegin\""], |v| {
        if v["message"]["type"] == "TurnBegin" {
            if s.started_ms == 0 {
                s.started_ms = ms_of(&v["timestamp"]).unwrap_or(0);
            }
            s.push_prompt(&text_of(&v["message"]["payload"]["user_input"]), lim);
        }
    })?;
    // sessions/<md5(work dir)>/<id>: kimi.json lists the work dirs.
    let hash = dir.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or_default();
    let hash = hash.rsplit('_').next().unwrap_or(hash);
    if let Some(root) = dir.parent().and_then(Path::parent).and_then(Path::parent) {
        if let Ok(meta) = read_json::<Value>(&root.join("kimi.json")) {
            let found = meta["work_dirs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|w| w["path"].as_str())
                .find(|p| hex_md5(p) == hash);
            s.cwd = found.unwrap_or_default().to_string();
        }
    }
    Ok(())
}

fn hex_md5(s: &str) -> String {
    Md5::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::common::testing::*;

    const LIM: Limits = Limits::TEST;

    #[test]
    fn reads_kimi_code() {
        let root = TempDir::new("kimi");
        let dir = root.join("sessions/wd_proj_1a2b3c4d5e6f/9d1c");
        std::fs::create_dir_all(dir.join("agents/main")).unwrap();
        let state = dir.join("state.json");
        std::fs::write(&state, r#"{"id":"9d1c","version":2,"title":"list the files here","titleKind":"replaceable","isCustomTitle":false,"createdAt":1762596005000,"updatedAt":1762596062000,"archived":false,"cwd":"/home/u/proj"}"#).unwrap();
        std::fs::write(
            dir.join("agents/main/wire.jsonl"),
            concat!(
                r#"{"type":"metadata","protocol_version":"1.5","created_at":1762596000000}"#,
                "\n",
                r#"{"type":"turn.prompt","agentId":"main","input":[{"type":"text","text":"list the files here"}],"origin":{"kind":"user"},"turnId":1,"time":1762596005000}"#,
                "\n",
                r#"{"type":"turn.prompt","agentId":"main","input":[{"type":"text","text":"skill body"}],"origin":{"kind":"skill"},"turnId":2}"#,
                "\n",
                r#"{"type":"turn.prompt","agentId":"main","input":[{"type":"text","text":"now add a README"}],"origin":{"kind":"user"},"turnId":3}"#,
                "\n"
            ),
        )
        .unwrap();
        assert_eq!(paths(list_files(&root)), vec![state.clone()]);
        let mut s = Session::default();
        parse_file(&state, &mut s, &LIM).unwrap();
        assert_eq!(s.id, "9d1c");
        assert_eq!(s.cwd, "/home/u/proj");
        assert_eq!(s.prompts, vec!["list the files here", "now add a README"]);
        assert_eq!(s.title(), "list the files here");
    }

    #[test]
    fn reads_legacy_kimi_cli() {
        let root = TempDir::new("kimi-old");
        let work = "/home/u/proj";
        let dir = root.join("sessions").join(hex_md5(work)).join("s-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(root.join("kimi.json"), format!(r#"{{"work_dirs":[{{"path":"{work}","kaos":"local"}}]}}"#))
            .unwrap();
        std::fs::write(dir.join("state.json"), r#"{"custom_title":"Files","archived":false}"#).unwrap();
        let wire = dir.join("wire.jsonl");
        std::fs::write(
            &wire,
            concat!(
                r#"{"type":"metadata","protocol_version":"1.10"}"#,
                "\n",
                r#"{"timestamp":1762596005.1,"message":{"type":"TurnBegin","payload":{"user_input":"list the files here"}}}"#,
                "\n",
                r#"{"timestamp":1762596009.2,"message":{"type":"TurnEnd","payload":{}}}"#,
                "\n",
                r#"{"timestamp":1762596062.0,"message":{"type":"TurnBegin","payload":{"user_input":[{"type":"text","text":"now add a README"}]}}}"#,
                "\n"
            ),
        )
        .unwrap();
        assert_eq!(paths(legacy_list_files(&root)), vec![wire.clone()]);
        let mut s = Session::default();
        legacy_parse_file(&wire, &mut s, &LIM).unwrap();
        assert_eq!(s.id, "s-1");
        assert_eq!(s.cwd, work);
        assert_eq!(s.started_ms, 1762596005100);
        assert_eq!(s.prompts, vec!["list the files here", "now add a README"]);
        assert_eq!(s.title(), "Files");
    }
}
