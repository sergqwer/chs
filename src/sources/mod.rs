//! Session formats. Each tool stores its history one of two ways:
//!
//! * **files** (`Storage::Files`) - parsed by the cached index in `index.rs`. A
//!   session is one main file plus, for some tools, companion files whose changes also
//!   invalidate the cache entry. Append-only formats (JSONL, aider's markdown) are read
//!   on from where the previous run stopped; the rest are parsed whole when they change;
//! * **one database for all sessions** (`Storage::Database`) - queried directly on
//!   every start (SQLite is fast at "newest N rows", no cache needed).
//!
//! Several tools share a format: Qoder, iFlow and Kode write Claude Code's transcript
//! layout, Kilo Code keeps OpenCode's database, so they are configured as sources of
//! those formats with their own folders.

pub mod aider;
pub mod amazonq;
pub mod antigravity;
pub mod auggie;
pub mod claude;
pub mod cline;
pub mod codebuddy;
pub mod codex;
pub mod common;
pub mod continue_cli;
pub mod copilot;
pub mod crush;
pub mod cursor;
pub mod droid;
pub mod dsh;
pub mod gemini;
pub mod goose;
pub mod hermes;
pub mod kimi;
pub mod opencode;
pub mod openhands;
pub mod pi;
pub mod qwen;
mod sqlite;
pub mod vibe;

use crate::config::Format;
use crate::session::{Limits, Session};
use common::Found;
use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    Files,
    Database,
}

pub fn storage(format: Format) -> Storage {
    use Format::*;
    match format {
        Opencode | Kilo | Crush | Goose | Amazonq | Hermes | Antigravity => Storage::Database,
        _ => Storage::Files,
    }
}

/// Whether the file is only ever appended to, so a later run may continue reading
/// where this one stopped (`parse`) instead of reading it whole (`parse_file`).
pub fn incremental(format: Format, path: &Path) -> bool {
    use Format::*;
    match format {
        Claude | Codex | Pi | Copilot | Qwen | Codebuddy | Aider => true,
        Gemini => gemini::incremental(path),
        Dsh => dsh::incremental(path),
        _ => false,
    }
}

/// The main file of every session below `root`, with its metadata.
pub fn list_files(format: Format, root: &Path) -> Vec<Found> {
    use Format::*;
    match format {
        Claude => claude::list_files(root),
        Codex => codex::list_files(root),
        Pi => pi::list_files(root),
        Copilot => copilot::list_files(root),
        Droid => droid::list_files(root),
        Cursor => cursor::list_files(root),
        Cline => cline::list_files(root),
        Openhands => openhands::list_files(root),
        Continue => continue_cli::list_files(root),
        Auggie => auggie::list_files(root),
        Aider => aider::list_files(root),
        Gemini => gemini::list_files(root),
        Qwen => qwen::list_files(root),
        Kimi => kimi::list_files(root),
        KimiCli => kimi::legacy_list_files(root),
        Vibe => vibe::list_files(root),
        Codebuddy => codebuddy::list_files(root),
        Dsh => dsh::list_files(root),
        Opencode | Kilo | Crush | Goose | Amazonq | Hermes | Antigravity => Vec::new(),
    }
}

/// Other files a whole-file session is read from: their size and mtime count towards
/// the cache fingerprint (missing ones are simply skipped). Only asked for sessions
/// the index actually looks at, since some (Cursor) cost a small read.
pub fn companions(format: Format, path: &Path) -> Vec<PathBuf> {
    use Format::*;
    match format {
        Cursor => cursor::companions(path),
        Cline => cline::companions(path),
        Openhands => openhands::companions(path),
        Kimi => kimi::companions(path),
        KimiCli => kimi::legacy_companions(path),
        Vibe => vibe::companions(path),
        _ => Vec::new(),
    }
}

/// Continue an incremental parse of `path`; returns the bytes consumed.
pub fn parse(format: Format, path: &Path, reader: impl BufRead, s: &mut Session, lim: &Limits) -> std::io::Result<u64> {
    use Format::*;
    match format {
        Claude => claude::parse(reader, s, lim),
        Codex => codex::parse(reader, s, lim),
        Pi => pi::parse(reader, s, lim),
        Copilot => copilot::parse(reader, s, lim),
        Qwen => qwen::parse(reader, s, lim),
        Codebuddy => codebuddy::parse(reader, s, lim),
        Dsh => dsh::parse(reader, s, lim),
        Aider => {
            let n = aider::parse(reader, s, lim)?;
            aider::locate(path, s);
            Ok(n)
        }
        Gemini => {
            let n = gemini::parse(reader, s, lim)?;
            gemini::locate(path, s);
            Ok(n)
        }
        _ => Ok(0),
    }
}

/// Read a whole-file session from scratch; `companions` is what `companions()` gave.
pub fn parse_file(
    format: Format,
    path: &Path,
    companions: &[PathBuf],
    s: &mut Session,
    lim: &Limits,
) -> std::io::Result<()> {
    use Format::*;
    match format {
        Cursor => cursor::parse_file(path, companions.first().map(PathBuf::as_path), s, lim),
        Cline => cline::parse_file(path, s, lim),
        Openhands => openhands::parse_file(path, s, lim),
        Continue => continue_cli::parse_file(path, s, lim),
        Auggie => auggie::parse_file(path, s, lim),
        Gemini => {
            gemini::parse_file(path, s, lim)?;
            gemini::locate(path, s);
            Ok(())
        }
        Kimi => kimi::parse_file(path, s, lim),
        KimiCli => kimi::legacy_parse_file(path, s, lim),
        Vibe => vibe::parse_file(path, s, lim),
        Dsh => dsh::parse_file(path, s, lim),
        Droid => droid::parse_file(path, s, lim),
        _ => Ok(()),
    }
}

/// Prompts of database sessions from the previous run. A database is queried on every
/// start, but a row whose update time has not moved keeps the prompts read last time
/// instead of having its messages read and parsed again.
pub struct Reuse<'a> {
    old: &'a HashMap<String, Session>,
    format: Format,
}

impl<'a> Reuse<'a> {
    pub fn new(old: &'a HashMap<String, Session>, format: Format) -> Reuse<'a> {
        Reuse { old, format }
    }

    pub fn key(format: Format, s: &Session) -> String {
        format!("{format:?}|{}|{}", s.file.to_string_lossy(), s.id)
    }

    /// Fill `s` with the cached prompts; false when they have to be read.
    pub fn fill(&self, s: &mut Session) -> bool {
        match self.old.get(&Self::key(self.format, s)) {
            Some(p) if s.updated_ms != 0 && p.updated_ms == s.updated_ms => {
                s.prompts.clone_from(&p.prompts);
                s.prompt_count = p.prompt_count;
                s.prompt_chars = p.prompt_chars;
                true
            }
            _ => false,
        }
    }
}

/// Newest `limit` sessions of a database-backed source, newest first.
pub fn load_database(format: Format, root: &Path, lim: &Limits, limit: usize, reuse: &Reuse) -> Vec<Session> {
    match format {
        Format::Opencode => opencode::load(root, "opencode.db", lim, limit, reuse),
        Format::Kilo => opencode::load(root, "kilo.db", lim, limit, reuse),
        Format::Crush => crush::load(root, lim, limit, reuse),
        Format::Goose => goose::load(root, lim, limit, reuse),
        // One row per folder, and no update time to tell a changed row by.
        Format::Amazonq => amazonq::load(root, lim, limit),
        Format::Hermes => hermes::load(root, lim, limit, reuse),
        Format::Antigravity => antigravity::load(root, lim, limit, reuse),
        _ => Vec::new(),
    }
}

/// Turn the raw (cached) parse state into what is displayed and searched. Runs on
/// every load, so it must stay cheap: anything that reads a file belongs in the parse,
/// whose result is cached.
pub fn finish(format: Format, s: &mut Session, lim: &Limits) {
    if format == Format::Codex {
        codex::finish(s, lim);
    }
    if s.id.is_empty() {
        s.id = id_from_path(format, &s.file);
    }
}

/// Whether `finish` would make the raw parse state `s` of `path` worth listing,
/// without the copy that finishing it takes.
pub fn useful(format: Format, s: &Session, path: &Path) -> bool {
    let id = !s.id.is_empty() || !id_from_path(format, path).is_empty();
    id && (s.has_content() || !s.alt_prompts.is_empty())
}

/// Fallback when a file never states its session id; empty where the name is not one.
fn id_from_path(format: Format, path: &Path) -> String {
    use Format::*;
    let stem = path.file_stem().and_then(|x| x.to_str()).unwrap_or_default();
    match format {
        // <id>.jsonl
        Claude | Droid | Qwen | Codebuddy => stem.to_string(),
        // rollout-<timestamp>-<uuid>: the uuid is the last 5 dash-separated groups.
        Codex => {
            let parts: Vec<&str> = stem.split('-').collect();
            if parts.len() >= 5 {
                parts[parts.len() - 5..].join("-")
            } else {
                String::new()
            }
        }
        // <timestamp>_<uuid>.jsonl
        Pi => stem.rsplit_once('_').map(|(_, id)| id.to_string()).unwrap_or_default(),
        // session-state/<uuid>/events.jsonl
        Copilot => path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or_default().to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_from_file_names() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(
            id_from_path(
                Format::Codex,
                &p("/x/rollout-2026-11-08T10-00-00-0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b.jsonl")
            ),
            "0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b"
        );
        assert_eq!(id_from_path(Format::Pi, &p("/x/2026-11-08T10-00-00-000Z_0b6f-11.jsonl")), "0b6f-11");
        assert_eq!(id_from_path(Format::Copilot, &p("/x/session-state/7f3c/events.jsonl")), "7f3c");
        assert_eq!(id_from_path(Format::Gemini, &p("/x/session-2026-11-08T10-00-3f2a1b2c.jsonl")), "");
    }
}
