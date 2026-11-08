//! Aider: `.aider.chat.history.md` in the root of each repository (the folder aider
//! ran in, without git). There is no global list of repositories, so the files are
//! looked for in the configured folders (a few levels deep) and in the folders of
//! every session the other sources found.
//!
//! The file is markdown, appended to: each run opens with `# aider chat started at
//! <local time>`, a prompt is a run of `#### ` lines (one per line of a multi-line
//! prompt), tool output is quoted with `> `. `aider --restore-chat-history` loads the
//! whole file, so one file is one entry here; its title is the first prompt of the
//! latest run.

use super::common::Found;
use crate::session::{Limits, Session};
use std::io::BufRead;
use std::path::Path;

pub const FILE: &str = ".aider.chat.history.md";
/// Folders below a configured root that are searched.
const DEPTH: u8 = 3;
const SKIP: [&str; 6] = ["node_modules", "target", "venv", "__pycache__", "dist", "build"];

pub fn list_files(root: &Path) -> Vec<Found> {
    let mut out = Vec::new();
    find(root, DEPTH, &mut out);
    out
}

/// One directory listing per folder finds both the history file and the subfolders.
fn find(dir: &Path, depth: u8, out: &mut Vec<Found>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        // file_type() does not follow symlinks, so linked trees are not walked twice.
        let Ok(ft) = e.file_type() else { continue };
        let name = e.file_name();
        if ft.is_file() && name == FILE {
            if let Ok(md) = e.metadata() {
                out.push(Found { path: e.path(), md });
            }
        } else if ft.is_dir() && depth > 0 {
            let name = name.to_string_lossy();
            if !name.starts_with('.') && !SKIP.contains(&name.as_ref()) {
                find(&e.path(), depth - 1, out);
            }
        }
    }
}

/// `s.summary` doubles as the title of the latest run: a header sets it to "" (waiting
/// for that run's first prompt), and it survives in the cache between partial reads.
pub fn parse(mut reader: impl BufRead, s: &mut Session, lim: &Limits) -> std::io::Result<u64> {
    let mut buf = Vec::with_capacity(4096);
    let mut consumed = 0u64;
    let mut prompt: Vec<String> = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            break;
        }
        consumed += n as u64;
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end();
        if let Some(text) = line.strip_prefix("#### ").or((line == "####").then_some("")) {
            prompt.push(text.to_string());
            continue;
        }
        flush(&mut prompt, s, lim);
        if let Some(ts) = line.strip_prefix("# aider chat started at ") {
            if s.started_ms == 0 {
                s.started_ms = super::common::ms_of_str(ts).unwrap_or(0);
            }
            s.summary = Some(String::new());
        }
    }
    flush(&mut prompt, s, lim);
    Ok(consumed)
}

fn flush(lines: &mut Vec<String>, s: &mut Session, lim: &Limits) {
    if lines.is_empty() {
        return;
    }
    let text = std::mem::take(lines).join("\n");
    let text = text.trim();
    // Commands are not prompts, except the ones that carry one.
    let text = match text.strip_prefix('/') {
        None => text,
        Some(cmd) => match cmd.split_once(char::is_whitespace) {
            Some(("ask" | "code" | "architect" | "context", rest)) => rest.trim(),
            _ => return,
        },
    };
    if text.is_empty() || text == "<blank>" {
        return;
    }
    if s.summary.as_deref() == Some("") {
        s.summary = Some(text.to_string());
    }
    s.push_prompt(text, lim);
}

/// The repository is the folder holding the file; the file path is the id.
pub fn locate(path: &Path, s: &mut Session) {
    s.id = path.to_string_lossy().into_owned();
    s.cwd = path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::common::testing::*;

    const LIM: Limits = Limits::TEST;

    #[test]
    fn reads_runs() {
        let text =
            "\n# aider chat started at 2026-11-08 10:15:02\n\n> Aider v0.86.1  \n#### add a --verbose flag  \n\n\
                    Done, edited main.py.\n\n> Applied edit to main.py  \n\n#### /add x.py  \n\n#### now add a test  \n\
                    #### use pytest  \n\n# aider chat started at 2026-11-09 09:00:00\n\n#### /ask why is it slow  \n\n\
                    #### <blank>  \n";
        let mut s = Session::default();
        let n = parse(text.as_bytes(), &mut s, &LIM).unwrap();
        assert_eq!(n as usize, text.len());
        assert_eq!(s.prompts, vec!["add a --verbose flag", "now add a test\nuse pytest", "why is it slow"]);
        assert_eq!(s.title(), "why is it slow", "the latest run names the entry");
        assert!(s.started_ms > 0);
    }

    #[test]
    fn appended_run_keeps_waiting_title() {
        let mut s = Session::default();
        let a = "# aider chat started at 2026-11-08 10:15:02\n\n";
        parse(a.as_bytes(), &mut s, &LIM).unwrap();
        assert_eq!(s.title(), "");
        parse("#### first one  \n".as_bytes(), &mut s, &LIM).unwrap();
        assert_eq!(s.title(), "first one");
    }

    #[test]
    fn finds_files_in_subfolders() {
        let root = TempDir::new("aider");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::write(root.join("a/b").join(FILE), "").unwrap();
        std::fs::write(root.join("node_modules/x").join(FILE), "").unwrap();
        assert_eq!(paths(list_files(&root)), vec![root.join("a/b").join(FILE)]);
    }
}
