//! Finding sessions, with an on-disk cache so a start costs a `stat` per file.
//!
//! For every session file the cache stores its size, mtime, a hash of its first bytes
//! and how far it was read. On the next start an unchanged file is reused as is; an
//! append-only file that only grew is read from the stored offset; anything else is
//! parsed again from the top. Sessions made of several files (a manifest plus a
//! transcript, a folder of events) fingerprint all of them.

use crate::config::{Config, Format};
use crate::session::{Limits, Session};
use crate::sources::{self, Storage};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Bump when the cached `Session` shape or parsing rules change.
const CACHE_VERSION: u32 = 4;
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
    /// Of the file, plus its companions' for whole-file formats.
    size: u64,
    mtime_ns: u64,
}

/// Everything the UI needs, newest first.
pub struct Loaded {
    /// All sessions reached, headless ones included (their `headless` flag set).
    pub sessions: Vec<Session>,
}

/// What a load accumulates while it works through the candidates.
struct Acc {
    entries: HashMap<String, Entry>,
    sessions: Vec<Session>,
    /// Per source: listed sessions so far, and candidates not yet looked at.
    visible: Vec<usize>,
    remaining: Vec<usize>,
    changed: bool,
}

fn mtime_ns(md: &std::fs::Metadata) -> u64 {
    md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos() as u64).unwrap_or(0)
}

fn candidate(source: usize, format: Format, path: PathBuf) -> Option<Candidate> {
    let md = std::fs::metadata(&path).ok()?;
    let (mut size, mut mtime) = (md.len(), mtime_ns(&md));
    // An appended file's size must stay its own: it is compared with the read offset.
    if !sources::incremental(format, &path) {
        for c in sources::companions(format, &path) {
            if let Ok(m) = std::fs::metadata(&c) {
                size += m.len();
                mtime = mtime.max(mtime_ns(&m));
            }
        }
    }
    Some(Candidate { source, format, path, size, mtime_ns: mtime })
}

pub fn load(cfg: &Config, ignore_cache: bool) -> Result<Loaded> {
    let lim = Limits { max_prompt_chars: cfg.search.max_prompt_chars, max_indexed_chars: cfg.search.max_indexed_chars };
    let cache_path = cfg.cache_dir().join("index.json");
    let old = if ignore_cache { CacheFile::default() } else { read_cache(&cache_path, &lim) };

    let mut cands = Vec::new();
    // Candidate folders may overlap (a tool's override variable pointing inside its
    // default folder): a file is listed once, for the first source that reaches it.
    let mut seen = HashSet::new();
    for (i, src) in cfg.sources.iter().enumerate() {
        // Aider waits for the second round, when the other sessions' folders are known.
        if !src.enabled || sources::storage(src.format) == Storage::Database || src.format == Format::Aider {
            continue;
        }
        for path in src.roots().iter().flat_map(|r| sources::list_files(src.format, r)) {
            if seen.insert(path.clone()) {
                cands.extend(candidate(i, src.format, path));
            }
        }
    }

    let markers: Vec<Vec<String>> = cfg.sources.iter().map(|s| s.headless_markers()).collect();
    let n = cfg.sources.len();
    let mut acc = Acc {
        entries: HashMap::new(),
        sessions: Vec::new(),
        visible: vec![0; n],
        remaining: vec![0; n],
        changed: old.entries.is_empty(),
    };
    ingest(cfg, &mut cands, &old, &lim, &markers, &mut acc);

    // Database-backed sources answer "newest N" themselves; take enough rows that the
    // headless ones (hidden by default) do not eat the visible pool.
    for (i, src) in cfg.sources.iter().enumerate() {
        if !src.enabled || sources::storage(src.format) != Storage::Database {
            continue;
        }
        let pool = cfg.general.search_pool * 3;
        let found =
            src.roots().iter().flat_map(|r| sources::load_database(src.format, r, &lim, pool)).collect::<Vec<_>>();
        for mut s in found {
            if !s.is_useful() {
                continue;
            }
            s.source = src.name.clone();
            s.headless = s.entry.as_ref().is_some_and(|e| markers[i].iter().any(|m| m == e));
            acc.sessions.push(s);
        }
    }

    // Aider keeps its history inside each repository: its configured folders plus the
    // folder of every session found so far, ranked together.
    let mut extra = aider_candidates(cfg, &acc.sessions);
    ingest(cfg, &mut extra, &old, &lim, &markers, &mut acc);

    // Merge all kinds: newest first (stable, so equal times keep file order).
    acc.sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));

    let changed = acc.changed || acc.entries.len() != old.entries.len();
    if changed {
        let file = CacheFile {
            version: CACHE_VERSION,
            limits: (lim.max_prompt_chars, lim.max_indexed_chars),
            entries: acc.entries,
        };
        // A cache that can not be written only costs speed next time.
        let _ = write_cache(&cache_path, &file);
    }
    Ok(Loaded { sessions: acc.sessions })
}

/// Parse candidates newest first until every source has a full pool of its own (or
/// runs out of files), so filtering to one source shows its 200 latest, not what is
/// left of a shared 200.
fn ingest(
    cfg: &Config,
    cands: &mut [Candidate],
    old: &CacheFile,
    lim: &Limits,
    markers: &[Vec<String>],
    acc: &mut Acc,
) {
    cands.sort_by_key(|c| std::cmp::Reverse(c.mtime_ns));
    for c in cands.iter() {
        acc.remaining[c.source] += 1;
    }
    let pool = cfg.general.search_pool;
    for chunk in cands.chunks(BATCH) {
        if (0..cfg.sources.len()).all(|i| acc.visible[i] >= pool || acc.remaining[i] == 0) {
            break;
        }
        for c in chunk {
            acc.remaining[c.source] -= 1;
        }
        let results = parse_parallel(chunk, &old.entries, lim);
        for (cand, (entry, reparsed)) in chunk.iter().zip(results) {
            let Some(entry) = entry else { continue };
            acc.changed |= reparsed;
            let src = &cfg.sources[cand.source];
            let mut s = entry.session.clone();
            acc.entries.insert(key(cand), entry);
            // finish() falls back to the file name for a missing id, so set it first.
            s.file = cand.path.clone();
            sources::finish(cand.format, &mut s, lim);
            if !s.is_useful() {
                continue;
            }
            s.source = src.name.clone();
            s.updated_ms = (cand.mtime_ns / 1_000_000) as i64;
            s.headless = s.entry.as_ref().is_some_and(|e| markers[cand.source].iter().any(|m| m == e));
            if !s.headless || src.include_headless {
                acc.visible[cand.source] += 1;
            }
            acc.sessions.push(s);
        }
    }
}

/// Aider histories: below each aider source's folders, and in the folder of every
/// session in `sessions` (those go to the first aider source).
fn aider_candidates(cfg: &Config, sessions: &[Session]) -> Vec<Candidate> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let aider: Vec<usize> =
        (0..cfg.sources.len()).filter(|&i| cfg.sources[i].enabled && cfg.sources[i].format == Format::Aider).collect();
    for &i in &aider {
        for p in cfg.sources[i].roots().iter().flat_map(|r| sources::list_files(Format::Aider, r)) {
            if seen.insert(p.clone()) {
                out.extend(candidate(i, Format::Aider, p));
            }
        }
    }
    let Some(&i) = aider.first() else { return out };
    for s in sessions.iter().filter(|s| !s.cwd.is_empty()) {
        let p = Path::new(&s.cwd).join(sources::aider::FILE);
        if seen.insert(p.clone()) && p.is_file() {
            out.extend(candidate(i, Format::Aider, p));
        }
    }
    out
}

/// The format is part of the key: the same file read as another format (a source's
/// `format` corrected in the config) must not reuse the old parse.
fn key(c: &Candidate) -> String {
    format!("{:?}|{}", c.format, c.path.to_string_lossy())
}

/// Returns (entry, whether the file had to be read). `None` = unreadable.
fn parse_parallel(chunk: &[Candidate], old: &HashMap<String, Entry>, lim: &Limits) -> Vec<(Option<Entry>, bool)> {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
    let per = chunk.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = chunk
            .chunks(per)
            .map(|part| {
                let h = scope.spawn(move || part.iter().map(|c| refresh(c, old.get(&key(c)), lim)).collect::<Vec<_>>());
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
        if !sources::incremental(c.format, &c.path) {
            let mut session = Session::default();
            sources::parse_file(c.format, &c.path, &mut session, lim)?;
            return Ok(Entry { size: c.size, mtime_ns: c.mtime_ns, head: 0, offset: 0, tail: 0, session });
        }
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
            "[general]\ncache_dir = {cache:?}\nsearch_pool = 10\nbuiltin_sources = false\n\
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

    #[test]
    fn companion_change_invalidates_whole_file_entry() {
        let tmp = std::env::temp_dir().join(format!("chs-test-comp-{}", std::process::id()));
        let dir = tmp.join("data/sessions/s1");
        std::fs::create_dir_all(&dir).unwrap();
        let t = format!(
            "[general]\ncache_dir = {cache:?}\nbuiltin_sources = false\n\
             [[sources]]\nname='cline'\nformat='cline'\npath={root:?}\nresume_command=['cline','--id','{{id}}']\n",
            cache = tmp.join("cache").to_string_lossy(),
            root = tmp.join("data").to_string_lossy()
        );
        let cfg = Config::from_user_str(&t).unwrap();
        std::fs::write(dir.join("s1.json"), r#"{"session_id":"s1","cwd":"/p","interactive":true}"#).unwrap();
        let msgs = |texts: &[&str]| {
            let m: Vec<String> = texts
                .iter()
                .map(|t| format!(r#"{{"role":"user","content":[{{"type":"text","text":"{t}"}}]}}"#))
                .collect();
            std::fs::write(dir.join("s1.messages.json"), format!(r#"{{"messages":[{}]}}"#, m.join(","))).unwrap();
        };
        msgs(&["first"]);
        assert_eq!(load(&cfg, false).unwrap().sessions[0].prompts, vec!["first"]);
        // Only the transcript changes; the manifest is untouched.
        std::thread::sleep(std::time::Duration::from_millis(20));
        msgs(&["first", "second"]);
        assert_eq!(load(&cfg, false).unwrap().sessions[0].prompts, vec!["first", "second"]);
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
