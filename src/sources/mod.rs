pub mod claude;
pub mod codex;

use crate::config::Format;
use crate::session::{Limits, Session};
use std::io::BufRead;
use std::path::{Path, PathBuf};

pub fn list_files(format: Format, root: &Path) -> Vec<PathBuf> {
    match format {
        Format::Claude => claude::list_files(root),
        Format::Codex => codex::list_files(root),
    }
}

pub fn parse(format: Format, reader: impl BufRead, s: &mut Session, lim: &Limits) -> std::io::Result<u64> {
    match format {
        Format::Claude => claude::parse(reader, s, lim),
        Format::Codex => codex::parse(reader, s, lim),
    }
}

/// Turn the raw (cached) parse state into what is displayed and searched.
pub fn finish(format: Format, s: &mut Session, lim: &Limits) {
    if format == Format::Codex {
        codex::finish(s, lim);
    }
    if s.id.is_empty() {
        // Fall back to the file name: claude names files <id>.jsonl, codex
        // rollout-<timestamp>-<id>.jsonl where the id is the last 5 dash-groups.
        if let Some(stem) = s.file.file_stem().and_then(|x| x.to_str()) {
            s.id = match format {
                Format::Claude => stem.to_string(),
                Format::Codex => {
                    let parts: Vec<&str> = stem.split('-').collect();
                    if parts.len() >= 5 {
                        parts[parts.len() - 5..].join("-")
                    } else {
                        String::new()
                    }
                }
            };
        }
    }
}
