//! Session formats. Each tool stores its history one of two ways:
//!
//! * **one file per session** (`Storage::Files`) - parsed by the cached, incremental
//!   index in `index.rs`; `append_only` formats (JSONL) are read on from where the
//!   previous run stopped, whole-file JSON is re-read when it changes;
//! * **one database for all sessions** (`Storage::Database`) - queried directly on
//!   every start (SQLite is fast at "newest N rows", no cache needed).

pub mod claude;
pub mod codex;
pub mod crush;
pub mod goose;
pub mod opencode;
mod sqlite;

use crate::config::Format;
use crate::session::{Limits, Session};
use std::io::BufRead;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    Files { append_only: bool },
    Database,
}

pub fn storage(format: Format) -> Storage {
    match format {
        Format::Claude | Format::Codex => Storage::Files { append_only: true },
        Format::Opencode | Format::Crush | Format::Goose => Storage::Database,
    }
}

pub fn list_files(format: Format, root: &Path) -> Vec<PathBuf> {
    match format {
        Format::Claude => claude::list_files(root),
        Format::Codex => codex::list_files(root),
        Format::Opencode | Format::Crush | Format::Goose => Vec::new(),
    }
}

pub fn parse(format: Format, reader: impl BufRead, s: &mut Session, lim: &Limits) -> std::io::Result<u64> {
    match format {
        Format::Claude => claude::parse(reader, s, lim),
        Format::Codex => codex::parse(reader, s, lim),
        Format::Opencode | Format::Crush | Format::Goose => Ok(0),
    }
}

/// Newest `limit` sessions of a database-backed source.
pub fn load_database(format: Format, root: &Path, lim: &Limits, limit: usize) -> Vec<Session> {
    match format {
        Format::Opencode => opencode::load(root, lim, limit),
        Format::Crush => crush::load(root, lim, limit),
        Format::Goose => goose::load(root, lim, limit),
        Format::Claude | Format::Codex => Vec::new(),
    }
}

/// Turn the raw (cached) parse state into what is displayed and searched.
pub fn finish(format: Format, s: &mut Session, lim: &Limits) {
    if format == Format::Codex {
        codex::finish(s, lim);
    }
    if s.id.is_empty() {
        if let Some(stem) = s.file.file_stem().and_then(|x| x.to_str()) {
            s.id = id_from_file_stem(format, stem);
        }
    }
}

/// Fallback when a file never states its session id.
fn id_from_file_stem(format: Format, stem: &str) -> String {
    match format {
        // <id>.jsonl
        Format::Claude => stem.to_string(),
        // rollout-<timestamp>-<uuid>: the uuid is the last 5 dash-separated groups.
        Format::Codex => {
            let parts: Vec<&str> = stem.split('-').collect();
            if parts.len() >= 5 {
                parts[parts.len() - 5..].join("-")
            } else {
                String::new()
            }
        }
        Format::Opencode | Format::Crush | Format::Goose => String::new(),
    }
}
