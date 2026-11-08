//! Small helpers shared by the formats that need no hand-tuned parser.

use memchr::memmem;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::io::BufRead;
use std::path::{Path, PathBuf};

/// Hand every complete line of `reader` that contains one of `needles` to `f`,
/// deserialized as `T` (lines that do not parse are skipped). Returns the bytes
/// consumed: a trailing line without its newline is still being written and is left
/// for the next run.
pub fn jsonl<T: DeserializeOwned>(
    mut reader: impl BufRead,
    needles: &[&str],
    mut f: impl FnMut(T),
) -> std::io::Result<u64> {
    let finders: Vec<memmem::Finder> = needles.iter().map(|n| memmem::Finder::new(n.as_bytes())).collect();
    let mut buf = Vec::with_capacity(64 * 1024);
    let mut consumed = 0u64;
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            break;
        }
        consumed += n as u64;
        if !finders.is_empty() && !finders.iter().any(|f| f.find(&buf).is_some()) {
            continue;
        }
        if let Ok(v) = serde_json::from_slice::<T>(&buf) {
            f(v);
        }
    }
    Ok(consumed)
}

/// A whole JSON file.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> std::io::Result<T> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}

/// The text of a message `content`: a plain string, or the `text` of every text part
/// of an array - `{"type": "text"}`, OpenAI's `input_text`, or Gemini's untyped
/// `{"text"}` (images, tool calls and tool results are skipped).
pub fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            let texts: Vec<&str> = parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str).is_none_or(|t| t == "text" || t == "input_text"))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect();
            texts.join("\n")
        }
        _ => String::new(),
    }
}

pub fn str_of(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

/// Milliseconds since the epoch from an RFC 3339 string, a naive "YYYY-MM-DD[T ]HH:MM:SS"
/// (taken as local time) or a number (seconds or milliseconds, told apart by size).
pub fn ms_of(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => {
            let x = n.as_f64()?;
            Some(if x < 1e11 { (x * 1000.0) as i64 } else { x as i64 })
        }
        Value::String(s) => ms_of_str(s),
        _ => None,
    }
}

pub fn ms_of_str(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(d.timestamp_millis());
    }
    // SQLite-style "2026-10-03 16:43:44.002530482+00:00".
    if let Ok(d) = chrono::DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f%:z") {
        return Some(d.timestamp_millis());
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(n) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            use chrono::TimeZone;
            return chrono::Local.from_local_datetime(&n).earliest().map(|d| d.timestamp_millis());
        }
    }
    None
}

/// A session file found while listing, with the metadata the listing already read
/// (on Windows a directory listing carries it for free), so it is stat-ed once.
pub struct Found {
    pub path: PathBuf,
    pub md: std::fs::Metadata,
}

/// `path` if it is a file.
pub fn stat(path: PathBuf) -> Option<Found> {
    let md = std::fs::metadata(&path).ok()?;
    md.is_file().then_some(Found { path, md })
}

/// Subfolders of `dir` (symlinked folders are not followed).
pub fn subdirs(dir: &Path) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
}

/// Files below `dir`, down to `depth` levels of subfolders, whose name passes `keep`.
pub fn walk(dir: &Path, depth: u8, keep: &dyn Fn(&str) -> bool, out: &mut Vec<Found>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth > 0 {
                walk(&e.path(), depth - 1, keep, out);
            }
        } else if ft.is_file() && e.file_name().to_str().is_some_and(keep) {
            if let Ok(md) = e.metadata() {
                out.push(Found { path: e.path(), md });
            }
        }
    }
}

/// Files directly in `dir` whose name passes `keep`.
pub fn files(dir: &Path, keep: &dyn Fn(&str) -> bool) -> Vec<Found> {
    let mut out = Vec::new();
    walk(dir, 0, keep, &mut out);
    out
}

/// Files directly in every subfolder of `dir`: `<dir>/*/<file>`.
pub fn files_in_subdirs(dir: &Path, keep: &dyn Fn(&str) -> bool) -> Vec<Found> {
    let mut out = Vec::new();
    for d in subdirs(dir) {
        walk(&d, 0, keep, &mut out);
    }
    out
}

/// `<dir>/*/<name>`: one folder per session holding a file of a fixed name.
pub fn per_folder(dir: &Path, name: &str) -> Vec<Found> {
    subdirs(dir).filter_map(|d| stat(d.join(name))).collect()
}

/// A `file://` URI (as IDE-side writers store folders) to a plain path:
/// `file:///home/u`, `file://localhost/home/u`, `file:///c%3A/x` (-> `c:/x`) and
/// `file://server/share` (-> `//server/share`, a UNC path).
pub fn uri_to_path(s: &str) -> String {
    let Some(rest) = s.strip_prefix("file://") else { return s.to_string() };
    let rest = match rest.strip_prefix("localhost") {
        Some(r) if r.starts_with('/') => r,
        _ => rest,
    };
    if !rest.starts_with('/') {
        return format!("//{}", decode(rest));
    }
    let p = decode(rest);
    // file:///c:/x -> c:/x on Windows.
    match p.as_bytes() {
        [b'/', d, b':', ..] if d.is_ascii_alphabetic() => p[1..].to_string(),
        _ => p,
    }
}

/// Percent-decoding.
fn decode(rest: &str) -> String {
    let mut bytes = Vec::with_capacity(rest.len());
    let raw = rest.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            if let Ok(b) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                bytes.push(b);
                i += 3;
                continue;
            }
        }
        bytes.push(raw[i]);
        i += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Helpers for the readers' tests.
#[cfg(test)]
pub mod testing {
    use super::Found;
    use std::path::{Path, PathBuf};

    /// A fresh folder under the system temp dir, removed when dropped (also when the
    /// test fails).
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(name: &str) -> TempDir {
            let p = std::env::temp_dir().join(format!("chs-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }

    impl std::ops::Deref for TempDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl AsRef<Path> for TempDir {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Write `text` to `path`, creating its folders.
    pub fn put(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// No prompts from a previous run: every database row is read.
    pub fn fresh(format: crate::config::Format) -> crate::sources::Reuse<'static> {
        static EMPTY: std::sync::LazyLock<std::collections::HashMap<String, crate::session::Session>> =
            std::sync::LazyLock::new(Default::default);
        crate::sources::Reuse::new(&EMPTY, format)
    }

    pub fn paths(found: Vec<Found>) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = found.into_iter().map(|f| f.path).collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_text() {
        let v: Value =
            serde_json::from_str(r#"[{"type":"text","text":"a"},{"type":"image"},{"type":"text","text":"b"}]"#)
                .unwrap();
        assert_eq!(text_of(&v), "a\nb");
        assert_eq!(text_of(&Value::String("x".into())), "x");
    }

    #[test]
    fn timestamps() {
        assert_eq!(ms_of(&serde_json::json!(1_700_000_000)), Some(1_700_000_000_000));
        assert_eq!(ms_of(&serde_json::json!(1_700_000_000_123i64)), Some(1_700_000_000_123));
        assert_eq!(ms_of_str("2026-11-08T10:00:00Z"), Some(1_794_132_000_000));
        assert!(ms_of_str("2026-11-08 10:00:00").is_some());
        assert!(ms_of_str("2026-11-08T10:00:00.1").is_some());
        assert_eq!(ms_of_str("2026-11-08 10:00:00.5+00:00"), Some(1_794_132_000_500));
        assert_eq!(ms_of_str("soon"), None);
    }

    #[test]
    fn uris() {
        assert_eq!(uri_to_path("file:///home/u/my%20app"), "/home/u/my app");
        assert_eq!(uri_to_path("file:///c%3A/work"), "c:/work");
        assert_eq!(uri_to_path("/plain"), "/plain");
        assert_eq!(uri_to_path("file://localhost/Users/u/repo"), "/Users/u/repo");
        assert_eq!(uri_to_path("file://server/share/repo"), "//server/share/repo");
    }
}
