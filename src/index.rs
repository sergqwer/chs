//! Finding sessions, with an on-disk cache so a start costs a `stat` per file.
//!
//! Session files are append-only JSONL. For every file the cache stores its size,
//! mtime, a hash of its first bytes and how far it was read. On the next start an
//! unchanged file is reused as is, a file that only grew is read from the stored
//! offset, and anything else is parsed again from the top.

use crate::config::{Config, Format};
use crate::session::{Limits, Session};
use crate::sources;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Bump when the cached `Session` shape or parsing rules change.
const CACHE_VERSION: u32 = 3;
const HASH_BYTES: usize = 512;
const BATCH: usize = 48;

#[derive(Serialize, Deserialize, Default)]
struct CacheFile {
    version: u32,
    /// Changing these limits changes what a parse keeps, so they invalidate the cache.
    limits: (usize, usize),
    entries: HashMap<String, Entry>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Entry {
    size: u64,
    mtime_ns: u64,
    head: u64,
    offset: u64,
    /// Hash of the bytes just before `offset`: an append is only trusted when they
    /// are unchanged, so a rewrite in place is caught even when the size grew.
    tail: u64,
    session: Session,
}

struct Candidate {
    source: usize,
    format: Format,
    path: PathBuf,
    size: u64,
    mtime_ns: u64,
}

/// Everything the UI needs, newest first.
pub struct Loaded {
    /// All sessions reached, headless ones included (their `headless` flag set).
    pub sessions: Vec<Session>,
}

pub fn load(cfg: &Config, ignore_cache: bool) -> Result<Loaded> {
    let lim = Limits { max_prompt_chars: cfg.search.max_prompt_chars, max_indexed_chars: cfg.search.max_indexed_chars };
    let cache_path = cfg.cache_dir().join("index.json");
    let old = if ignore_cache { CacheFile::default() } else { read_cache(&cache_path, &lim) };

    let mut cands = Vec::new();
    for (i, src) in cfg.sources.iter().enumerate() {
        if !src.enabled {
            continue;
        }
        for path in sources::list_files(src.format, &src.root()) {
            let Ok(md) = std::fs::metadata(&path) else { continue };
            let mtime_ns = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            cands.push(Candidate { source: i, format: src.format, path, size: md.len(), mtime_ns });
        }
    }
    cands.sort_by_key(|c| std::cmp::Reverse(c.mtime_ns));

    let markers: Vec<Vec<String>> = cfg.sources.iter().map(|s| s.headless_markers()).collect();
    let pool = cfg.general.search_pool;
    let mut new_entries: HashMap<String, Entry> = HashMap::new();
    let mut sessions = Vec::new();
    // Read until every source has a full pool of its own (or runs out of files), so
    // filtering to one source shows its 200 latest, not what is left of a shared 200.
    let mut visible = vec![0usize; cfg.sources.len()];
    let mut remaining = vec![0usize; cfg.sources.len()];
    for c in &cands {
        remaining[c.source] += 1;
    }
    let mut changed = old.entries.is_empty();

    for chunk in cands.chunks(BATCH) {
        if (0..cfg.sources.len()).all(|i| visible[i] >= pool || remaining[i] == 0) {
            break;
        }
        for c in chunk {
            remaining[c.source] -= 1;
        }
        let results = parse_parallel(chunk, &old.entries, &lim);
        for (cand, (entry, reparsed)) in chunk.iter().zip(results) {
            let Some(entry) = entry else { continue };
            changed |= reparsed;
            let src = &cfg.sources[cand.source];
            let mut s = entry.session.clone();
            new_entries.insert(key(&cand.path), entry);
            // finish() falls back to the file name for a missing id, so set it first.
            s.file = cand.path.clone();
            sources::finish(cand.format, &mut s, &lim);
            if !s.is_useful() {
                continue;
            }
            s.source = src.name.clone();
            s.updated_ms = (cand.mtime_ns / 1_000_000) as i64;
            s.headless = s.entry.as_ref().is_some_and(|e| markers[cand.source].iter().any(|m| m == e));
            if !s.headless || src.include_headless {
                visible[cand.source] += 1;
            }
            sessions.push(s);
        }
    }

    changed |= new_entries.len() != old.entries.len();
    if changed {
        let file = CacheFile {
            version: CACHE_VERSION,
            limits: (lim.max_prompt_chars, lim.max_indexed_chars),
            entries: new_entries,
        };
        // A cache that can not be written only costs speed next time.
        let _ = write_cache(&cache_path, &file);
    }
    Ok(Loaded { sessions })
}

fn key(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Returns (entry, whether the file had to be read). `None` = unreadable.
fn parse_parallel(chunk: &[Candidate], old: &HashMap<String, Entry>, lim: &Limits) -> Vec<(Option<Entry>, bool)> {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
    let per = chunk.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = chunk
            .chunks(per)
            .map(|part| {
                let h = scope
                    .spawn(move || part.iter().map(|c| refresh(c, old.get(&key(&c.path)), lim)).collect::<Vec<_>>());
                (part.len(), h)
            })
            .collect();
        // A parser thread that panicked must still yield one slot per file, or the
        // results would no longer line up with the candidates they belong to.
        handles
            .into_iter()
            .flat_map(|(n, h)| h.join().unwrap_or_else(|_| (0..n).map(|_| (None, true)).collect()))
            .collect()
    })
}

fn refresh(c: &Candidate, old: Option<&Entry>, lim: &Limits) -> (Option<Entry>, bool) {
    if let Some(e) = old {
        if e.size == c.size && e.mtime_ns == c.mtime_ns {
            return (Some(e.clone()), false);
        }
    }
    let r = (|| -> std::io::Result<Entry> {
        let mut f = File::open(&c.path)?;
        let head = hash_at(&mut f, 0, HASH_BYTES)?;
        // Continue from the old offset only for a pure append: the file grew, and both
        // its first bytes and the bytes up to where we stopped are what we saw before.
        let appended = match old {
            Some(e) if c.size > e.size && e.head == head && e.offset <= c.size => {
                tail_hash(&mut f, e.offset)? == e.tail
            }
            _ => false,
        };
        let (mut session, start) = match old {
            Some(e) if appended => (e.session.clone(), e.offset),
            _ => (Session::default(), 0),
        };
        f.seek(SeekFrom::Start(start))?;
        let consumed = sources::parse(c.format, BufReader::with_capacity(256 * 1024, &mut f), &mut session, lim)?;
        let offset = start + consumed;
        let tail = tail_hash(&mut f, offset)?;
        Ok(Entry { size: c.size, mtime_ns: c.mtime_ns, head, offset, tail, session })
    })();
    (r.ok(), true)
}

/// Hash of the (up to) HASH_BYTES bytes that end at `offset`.
fn tail_hash(f: &mut File, offset: u64) -> std::io::Result<u64> {
    let at = offset.saturating_sub(HASH_BYTES as u64);
    hash_at(f, at, (offset - at) as usize)
}

/// FNV-1a of up to `len` (<= HASH_BYTES) bytes starting at `at`.
fn hash_at(f: &mut File, at: u64, len: usize) -> std::io::Result<u64> {
    f.seek(SeekFrom::Start(at))?;
    let mut buf = [0u8; HASH_BYTES];
    let len = len.min(HASH_BYTES);
    let mut n = 0;
    while n < len {
        let k = f.read(&mut buf[n..len])?;
        if k == 0 {
            break;
        }
        n += k;
    }
    // FNV-1a: stable across builds, unlike std's randomly seeded hasher.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in &buf[..n] {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    Ok(h ^ n as u64)
}

fn read_cache(path: &Path, lim: &Limits) -> CacheFile {
    let Ok(bytes) = std::fs::read(path) else { return CacheFile::default() };
    match serde_json::from_slice::<CacheFile>(&bytes) {
        Ok(c) if c.version == CACHE_VERSION && c.limits == (lim.max_prompt_chars, lim.max_indexed_chars) => c,
        _ => CacheFile::default(),
    }
}

fn write_cache(path: &Path, file: &CacheFile) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(file)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn clear_cache(cfg: &Config) {
    let _ = std::fs::remove_file(cfg.cache_dir().join("index.json"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn test_cfg(root: &Path, cache: &Path) -> Config {
        let t = format!(
            "[general]\ncache_dir = {cache:?}\nsearch_pool = 10\n\
             [[sources]]\nname='claude'\nformat='claude'\npath={root:?}\nresume_command=['claude','--resume','{{id}}']\n",
            cache = cache.to_string_lossy(),
            root = root.to_string_lossy()
        );
        Config::from_user_str(&t).unwrap()
    }

    fn line(id: &str, text: &str, entry: &str) -> String {
        format!(
            "{{\"type\":\"user\",\"message\":{{\"content\":{text:?}}},\"sessionId\":\"{id}\",\"cwd\":\"/w\",\"entrypoint\":\"{entry}\"}}\n"
        )
    }

    #[test]
    fn incremental_append_and_headless() {
        let tmp = std::env::temp_dir().join(format!("chs-test-{}", std::process::id()));
        let proj = tmp.join("root/projects/-w");
        std::fs::create_dir_all(&proj).unwrap();
        let cache = tmp.join("cache");
        let cfg = test_cfg(&tmp.join("root"), &cache);

        let a = proj.join("a.jsonl");
        std::fs::write(&a, line("a", "first prompt", "cli")).unwrap();
        std::fs::write(proj.join("b.jsonl"), line("b", "automated", "sdk-cli")).unwrap();

        let l = load(&cfg, false).unwrap();
        assert_eq!(l.sessions.len(), 2);
        let a_s = l.sessions.iter().find(|s| s.id == "a").unwrap();
        assert!(!a_s.headless);
        assert!(l.sessions.iter().find(|s| s.id == "b").unwrap().headless);

        // Append: only the tail is read, earlier prompts survive from the cache.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut f = std::fs::OpenOptions::new().append(true).open(&a).unwrap();
        f.write_all(line("a", "second prompt", "cli").as_bytes()).unwrap();
        drop(f);
        let l = load(&cfg, false).unwrap();
        let a_s = l.sessions.iter().find(|s| s.id == "a").unwrap();
        assert_eq!(a_s.prompts, vec!["first prompt", "second prompt"]);
        assert_eq!(l.sessions[0].id, "a", "newest first");

        // Same size, changed after the first 512 bytes: must not keep the stale prompt.
        let pad = "x".repeat(600);
        std::fs::write(&a, line("a", &format!("{pad} one"), "cli")).unwrap();
        load(&cfg, false).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&a, line("a", &format!("{pad} two"), "cli")).unwrap();
        let l = load(&cfg, false).unwrap();
        assert!(l.sessions.iter().find(|s| s.id == "a").unwrap().prompts[0].ends_with("two"));

        // Rewritten from scratch (different head): parsed again from the top.
        std::fs::write(&a, line("a", "rewritten", "cli")).unwrap();
        let l = load(&cfg, false).unwrap();
        assert_eq!(l.sessions.iter().find(|s| s.id == "a").unwrap().prompts, vec!["rewritten"]);
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
