//! DeepSeek Harness (`dsh`): `$DSH_HOME/sessions` or `~/.dsh/sessions`, one folder per
//! session, `--<project key>--/<session id>/session.v<N>.jsonl.zstd` - an append-only
//! JSONL log compressed as a run of independent zstd frames (`.jsonl` without
//! compression). A format migration writes the next version beside the old one, so
//! only the highest `N` is read.
//!
//! * line 1, `{"type": "session", "id", "createdAt", "cwd", "origin"?, "agentPreset"?}`;
//! * `user/message` events: `data.content` parts, typed by a person when
//!   `data.source.kind == "user"` (runtime context, skills, hooks... come in as other
//!   kinds);
//! * `session/title`: `data.title`, from the provider, the user or a fallback (the
//!   first prompt).

use super::common::{jsonl, ms_of, str_of, text_of};
use crate::session::{Limits, Session};
use serde_json::Value;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};

pub fn list_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for project in std::fs::read_dir(root).into_iter().flatten().flatten() {
        if !project.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        for session in std::fs::read_dir(project.path()).into_iter().flatten().flatten() {
            if session.file_type().is_ok_and(|t| t.is_dir()) {
                out.extend(latest(&session.path()));
            }
        }
    }
    out
}

/// `session.jsonl` is generation 0, `session.v4.jsonl.zstd` generation 4.
fn generation(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("session")?;
    let rest = rest.strip_suffix(".zstd").unwrap_or(rest).strip_suffix(".jsonl")?;
    match rest {
        "" => Some(0),
        v => v.strip_prefix(".v")?.parse().ok(),
    }
}

fn latest(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| Some((generation(e.file_name().to_str()?)?, e.path())))
        .max_by_key(|(g, _)| *g)
        .map(|(_, p)| p)
}

pub fn incremental(path: &Path) -> bool {
    path.extension().is_some_and(|x| x == "jsonl")
}

const NEEDLES: [&str; 3] = ["\"session\"", "\"user/message\"", "\"session/title\""];

pub fn parse(reader: impl BufRead, s: &mut Session, lim: &Limits) -> std::io::Result<u64> {
    jsonl::<Value>(reader, &NEEDLES, |v| apply(&v, s, lim))
}

/// A compressed log, streamed through the decoder frame after frame; a frame still
/// being written at the end ends the stream with whatever it held.
pub fn parse_file(path: &Path, s: &mut Session, lim: &Limits) -> std::io::Result<()> {
    let file = std::io::BufReader::new(std::fs::File::open(path)?);
    let frames = Frames { decoder: None, source: Some(file) };
    parse(std::io::BufReader::with_capacity(256 * 1024, frames), s, lim).map(|_| ())
}

type Decoder<R> = ruzstd::decoding::StreamingDecoder<R, ruzstd::decoding::FrameDecoder>;

/// Concatenated zstd frames as one stream (ruzstd's decoder reads a single frame).
struct Frames<R: Read> {
    decoder: Option<Decoder<R>>,
    source: Option<R>,
}

impl<R: Read> Read for Frames<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if let Some(d) = &mut self.decoder {
                match d.read(buf) {
                    Ok(0) => self.source = self.decoder.take().map(Decoder::into_inner),
                    Ok(n) => return Ok(n),
                    // A torn last frame: what came before it stands.
                    Err(_) => return Ok(0),
                }
            }
            // No next frame (end of file or garbage) ends the stream.
            let Some(src) = self.source.take() else { return Ok(0) };
            match Decoder::new(src) {
                Ok(d) => self.decoder = Some(d),
                Err(_) => return Ok(0),
            }
        }
    }
}

fn apply(v: &Value, s: &mut Session, lim: &Limits) {
    let d = &v["data"];
    match v["type"].as_str() {
        Some("session") => {
            s.id = str_of(&v["id"]).unwrap_or_default();
            s.cwd = str_of(&v["cwd"]).unwrap_or_default();
            s.started_ms = ms_of(&v["createdAt"]).unwrap_or(0);
            if v["origin"] == "subagent" {
                s.entry = Some("subagent".into());
            }
        }
        Some("user/message") if d["source"]["kind"].as_str().is_none_or(|k| k == "user") => {
            s.push_prompt(&text_of(&d["content"]), lim);
        }
        Some("session/title") => match d["source"]["kind"].as_str() {
            Some("user") => s.custom_title = str_of(&d["title"]),
            Some("provider") => s.ai_title = str_of(&d["title"]),
            _ => {}
        },
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: [&str; 6] = [
        r#"{"type":"session","version":4,"id":"session-3f6c","createdAt":1794045600000,"cwd":"/home/u/proj","isSeeded":false,"delegationDepth":0,"agentPreset":"standard"}"#,
        r#"{"type":"user/message","seq":3,"time":1794045601003,"data":{"content":[{"type":"text","text":"fix the flaky parser test"}],"source":{"kind":"user"},"role":"user","id":"m2"}}"#,
        r#"{"type":"user/message","seq":4,"time":1794045601004,"data":{"content":[{"type":"text","text":"Current runtime context."}],"source":{"kind":"runtime-context"},"role":"user","id":"m3"}}"#,
        r#"{"type":"session/title","seq":5,"time":1794045601005,"data":{"title":"fix the flaky parser test","source":{"kind":"fallback"}}}"#,
        r#"{"type":"session/title","seq":6,"time":1794045601006,"data":{"title":"Flaky parser test","source":{"kind":"provider"}}}"#,
        r#"{"type":"user/message","seq":10,"time":1794045700001,"data":{"content":[{"type":"text","text":"now add a regression test"}],"source":{"kind":"user"},"role":"user","id":"m5"}}"#,
    ];

    #[test]
    fn picks_latest_generation() {
        assert_eq!(generation("session.jsonl"), Some(0));
        assert_eq!(generation("session.v4.jsonl.zstd"), Some(4));
        assert_eq!(generation("session.lock"), None);
        assert_eq!(generation("session.migration.1.tmp"), None);
        let root = std::env::temp_dir().join(format!("chs-dsh-l-{}", std::process::id()));
        let dir = root.join("--home-u-proj--/session-3f6c");
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["session.v3.jsonl.zstd", "session.v4.jsonl.zstd", "session.lock"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        assert_eq!(list_files(&root), vec![dir.join("session.v4.jsonl.zstd")]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reads_multi_frame_zstd_with_torn_tail() {
        use ruzstd::encoding::{compress_to_vec, CompressionLevel};
        let mut bytes = Vec::new();
        // Header frame, then one frame per append batch, as dsh writes them.
        for batch in [&LOG[..1], &LOG[1..4], &LOG[4..]] {
            let text: String = batch.iter().map(|l| format!("{l}\n")).collect();
            bytes.extend(compress_to_vec(text.as_bytes(), CompressionLevel::Fastest));
        }
        let torn = compress_to_vec(&b"{\"type\":\"user/message\",\"data\":{}}\n"[..], CompressionLevel::Fastest);
        bytes.extend(&torn[..torn.len() / 2]);
        let dir = std::env::temp_dir().join(format!("chs-dsh-z-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("session.v4.jsonl.zstd");
        std::fs::write(&f, &bytes).unwrap();
        assert!(!incremental(&f));
        let lim = Limits { max_prompt_chars: 4000, max_indexed_chars: 40000 };
        let mut s = Session::default();
        parse_file(&f, &mut s, &lim).unwrap();
        assert_eq!(s.id, "session-3f6c");
        assert_eq!(s.cwd, "/home/u/proj");
        assert_eq!(s.prompts, vec!["fix the flaky parser test", "now add a regression test"]);
        assert_eq!(s.title(), "Flaky parser test");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
