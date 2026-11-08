//! Finding sessions, with an on-disk cache so a start costs a `stat` per file.
//!
//! For every session file the cache stores its size, mtime, a hash of its first bytes
//! and how far it was read. On the next start an unchanged file is reused as is; an
//! append-only file that only grew is read from the stored offset; anything else is
//! parsed again from the top. Sessions made of several files (a manifest plus a
//! transcript, a folder of events) fingerprint all of them.
//!
//! What is kept is bounded by what can be shown, not by how much history there is:
//! per source, the newest `search_pool` sessions (all of them, for Ctrl+A) plus the
//! newest `search_pool` it lists by default. Older sessions that had to be read to
//! get there (headless runs piling up in front of the next interactive one) stay in
//! the cache as a fingerprint and their headless marker only, so they are not read
//! again; files older than a source's last listed session are not read at all.

use crate::config::{Config, Format};
use crate::session::{Limits, Session};
use crate::sources::common::Found;
use crate::sources::{self, Reuse, Storage};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Bump when the cached `Session` shape or parsing rules change.
const CACHE_VERSION: u32 = 5;
const HASH_BYTES: usize = 512;
const BATCH: usize = 48;

#[derive(Serialize, Deserialize, Default)]
struct CacheFile {
    version: u32,
    /// Changing these limits changes what a parse keeps, so they invalidate the cache.
    limits: (usize, usize),
    entries: HashMap<String, Entry>,
    /// Database-backed sessions by `Reuse::key`: their prompts are reused while a
    /// row's update time stays the same.
    #[serde(default)]
    db: HashMap<String, Session>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    size: u64,
    mtime_ns: u64,
    head: u64,
    offset: u64,
    /// Hash of the bytes just before `offset`: an append is only trusted when they
    /// are unchanged, so a rewrite in place is caught even when the size grew.
    tail: u64,
    /// Only the headless marker is kept (see the module docs).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    slim: bool,
    session: Session,
}

impl Entry {
    /// Keep only what decides whether the session is headless.
    fn slim(mut self) -> Entry {
        if !self.slim {
            self.session = Session { entry: self.session.entry.take(), ..Default::default() };
            self.slim = true;
        }
        self
    }
}

struct Candidate {
    source: usize,
    format: Format,
    path: PathBuf,
    /// The cache key. The format is part of it: the same file read as another format
    /// (a source's `format` corrected in the config) must not reuse the old parse.
    key: String,
    /// Of the main file alone: companions are only looked at when the candidate is.
    size: u64,
    mtime_ns: u64,
    incremental: bool,
}

impl Candidate {
    fn new(source: usize, format: Format, f: Found) -> Candidate {
        let incremental = sources::incremental(format, &f.path);
        let key = format!("{format:?}|{}", f.path.to_string_lossy());
        Candidate { source, format, size: f.md.len(), mtime_ns: mtime_ns(&f.md), path: f.path, key, incremental }
    }
}

/// Everything the UI needs, newest first.
pub struct Loaded {
    /// The sessions that can be shown, headless ones included (`headless` set).
    pub sessions: Vec<Session>,
    /// `sessions[i].title()`, worked out once.
    pub titles: Vec<String>,
}

/// Flag sessions whose folder is gone: one stat per distinct folder.
fn mark_missing_folders(sessions: &mut [Session]) {
    let mut seen: HashMap<String, bool> = HashMap::new();
    for s in sessions.iter_mut().filter(|s| !s.cwd.is_empty()) {
        s.cwd_missing = !*seen.entry(s.cwd.clone()).or_insert_with(|| Path::new(&s.cwd).is_dir());
    }
}

/// Indices of the sessions a list starts from (the TUI and `chs -l` alike): one
/// source or all of them, headless ones only when asked for (or when their source
/// always shows them), at most `search_pool`, newest first.
pub fn pool(cfg: &Config, sessions: &[Session], source: Option<usize>, show_headless: bool) -> Vec<usize> {
    sessions
        .iter()
        .enumerate()
        .filter(|(_, s)| source.is_none_or(|f| f == s.src))
        .filter(|(_, s)| !s.headless || show_headless || cfg.sources[s.src].include_headless)
        .map(|(i, _)| i)
        .take(cfg.general.search_pool)
        .collect()
}

/// Per source: how many sessions were admitted so far, in all and without the
/// headless ones a source hides.
struct Window {
    pool: usize,
    all: Vec<usize>,
    visible: Vec<usize>,
}

impl Window {
    /// Whether the next (older) session of `src` can ever be shown - it is among the
    /// newest `pool` of the source, or among the newest `pool` the source lists by
    /// default - and count it.
    fn admit(&mut self, src: usize, visible: bool) -> bool {
        let keep = self.all[src] < self.pool || (visible && self.visible[src] < self.pool);
        self.all[src] += 1;
        if visible {
            self.visible[src] += 1;
        }
        keep
    }

    /// Nothing older from `src` can be shown any more.
    fn full(&self, src: usize) -> bool {
        self.visible[src] >= self.pool
    }
}

/// What a load accumulates while it works through the candidates.
struct Acc {
    entries: HashMap<String, Entry>,
    sessions: Vec<Session>,
    win: Window,
    changed: bool,
}

fn mtime_ns(md: &std::fs::Metadata) -> u64 {
    md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos() as u64).unwrap_or(0)
}

pub fn load(cfg: &Config, ignore_cache: bool) -> Result<Loaded> {
    let lim = cfg.limits();
    let cache_path = cfg.cache_dir().join("index.json");
    let mut old = if ignore_cache { CacheFile::default() } else { read_cache(&cache_path, &lim) };
    let roots: Vec<Vec<PathBuf>> = cfg.sources.iter().map(|s| if s.enabled { s.roots() } else { Vec::new() }).collect();
    let markers: Vec<Vec<String>> = cfg.sources.iter().map(|s| s.headless_markers()).collect();
    let n = cfg.sources.len();
    let mut acc = Acc {
        entries: HashMap::new(),
        sessions: Vec::new(),
        win: Window { pool: cfg.general.search_pool, all: vec![0; n], visible: vec![0; n] },
        changed: old.entries.is_empty(),
    };

    let mut cands = Vec::new();
    // Candidate folders may overlap (a tool's override variable pointing inside its
    // default folder): a file is listed once, for the first source that reaches it.
    let mut seen = HashSet::new();
    for (i, src) in cfg.sources.iter().enumerate() {
        // Aider waits for the second round, when the other sessions' folders are known.
        if sources::storage(src.format) == Storage::Database || src.format == Format::Aider {
            continue;
        }
        for f in roots[i].iter().flat_map(|r| sources::list_files(src.format, r)) {
            if seen.insert(f.path.clone()) {
                cands.push(Candidate::new(i, src.format, f));
            }
        }
    }
    ingest(cfg, cands, &mut old.entries, &lim, &markers, &mut acc);

    // Database-backed sources answer "newest N" themselves; take enough rows that the
    // headless ones (hidden by default) do not eat the visible pool.
    let mut db = HashMap::new();
    for (i, src) in cfg.sources.iter().enumerate() {
        if sources::storage(src.format) != Storage::Database {
            continue;
        }
        let reuse = Reuse::new(&old.db, src.format);
        let rows = cfg.general.search_pool * 3;
        let mut found: Vec<Session> =
            roots[i].iter().flat_map(|r| sources::load_database(src.format, r, &lim, rows, &reuse)).collect();
        if roots[i].len() > 1 {
            found.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
        }
        for mut s in found.into_iter().filter(Session::is_useful) {
            s.headless = s.entry.as_ref().is_some_and(|e| markers[i].iter().any(|m| m == e));
            if !acc.win.admit(i, !s.headless || src.include_headless) {
                continue;
            }
            let key = Reuse::key(src.format, &s);
            acc.changed |= old.db.get(&key).is_none_or(|o| o.updated_ms != s.updated_ms);
            s.source = src.name.clone();
            s.src = i;
            db.insert(key, s.clone());
            acc.sessions.push(s);
        }
    }
    acc.changed |= db.len() != old.db.len();

    // Aider keeps its history inside each repository: its configured folders plus the
    // folder of every session found so far, ranked together.
    let extra = aider_candidates(cfg, &roots, &acc.sessions);
    ingest(cfg, extra, &mut old.entries, &lim, &markers, &mut acc);

    // Merge all kinds: newest first (stable, so equal times keep file order).
    acc.sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));

    // Entries left over belong to files that are gone or too old to matter.
    if acc.changed || !old.entries.is_empty() {
        let file = CacheFile {
            version: CACHE_VERSION,
            limits: (lim.max_prompt_chars, lim.max_indexed_chars),
            entries: acc.entries,
            db,
        };
        // A cache that can not be written only costs speed next time.
        let _ = write_cache(&cache_path, &file);
    }
    mark_missing_folders(&mut acc.sessions);
    let titles = acc.sessions.iter().map(Session::title).collect();
    Ok(Loaded { sessions: acc.sessions, titles })
}

/// Work through `cands` newest first, in parallel batches, until every source has
/// all it can show (or runs out of files). The entries used are taken out of `old`.
fn ingest(
    cfg: &Config,
    mut cands: Vec<Candidate>,
    old: &mut HashMap<String, Entry>,
    lim: &Limits,
    markers: &[Vec<String>],
    acc: &mut Acc,
) {
    cands.sort_by_key(|c| std::cmp::Reverse(c.mtime_ns));
    let mut remaining = vec![0usize; cfg.sources.len()];
    for c in &cands {
        remaining[c.source] += 1;
    }
    let mut next = cands.iter();
    loop {
        if (0..cfg.sources.len()).all(|i| acc.win.full(i) || remaining[i] == 0) {
            break;
        }
        // The next batch, leaving out sources that already have all they can show.
        let mut batch: Vec<(&Candidate, Option<Entry>)> = Vec::with_capacity(BATCH);
        for c in next.by_ref() {
            remaining[c.source] -= 1;
            if !acc.win.full(c.source) {
                batch.push((c, old.remove(&c.key)));
                if batch.len() == BATCH {
                    break;
                }
            }
        }
        if batch.is_empty() {
            break;
        }
        let cs: Vec<&Candidate> = batch.iter().map(|(c, _)| *c).collect();
        for (c, (entry, reparsed)) in cs.iter().zip(parse_parallel(batch, lim)) {
            let Some(entry) = entry else { continue };
            acc.changed |= reparsed;
            take(cfg, c, entry, lim, markers, acc);
        }
    }
}

/// Decide what one parsed (or cached) session is: listed, kept slim, or skipped.
/// Only a listed one is copied out of its entry.
fn take(cfg: &Config, c: &Candidate, entry: Entry, lim: &Limits, markers: &[Vec<String>], acc: &mut Acc) {
    let src = &cfg.sources[c.source];
    let headless = entry.session.entry.as_ref().is_some_and(|m| markers[c.source].contains(m));
    // A slim entry was a listable session when it was slimmed.
    if !entry.slim && !sources::useful(c.format, &entry.session, &c.path) {
        acc.entries.insert(c.key.clone(), entry);
        return;
    }
    if !acc.win.admit(c.source, !headless || src.include_headless) {
        acc.changed |= !entry.slim;
        acc.entries.insert(c.key.clone(), entry.slim());
        return;
    }
    let entry = if entry.slim {
        // Wanted after all (a larger pool, headless sessions switched on): read it whole.
        let (Some(full), _) = refresh(c, None, lim) else { return };
        acc.changed = true;
        full
    } else {
        entry
    };
    let mut s = entry.session.clone();
    // finish() falls back to the file name for a missing id, so set it first.
    s.file = c.path.clone();
    sources::finish(c.format, &mut s, lim);
    s.source = src.name.clone();
    s.src = c.source;
    s.updated_ms = (entry.mtime_ns / 1_000_000) as i64;
    s.headless = headless;
    acc.entries.insert(c.key.clone(), entry);
    if s.is_useful() {
        acc.sessions.push(s);
    }
}

/// Aider histories: below each aider source's folders, and in the folder of every
/// session in `sessions` (those go to the first aider source).
fn aider_candidates(cfg: &Config, roots: &[Vec<PathBuf>], sessions: &[Session]) -> Vec<Candidate> {
    let aider: Vec<usize> = (0..cfg.sources.len()).filter(|&i| cfg.sources[i].format == Format::Aider).collect();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for &i in &aider {
        for f in roots[i].iter().flat_map(|r| sources::list_files(Format::Aider, r)) {
            if seen.insert(f.path.clone()) {
                out.push(Candidate::new(i, Format::Aider, f));
            }
        }
    }
    let Some(&i) = aider.iter().find(|&&i| cfg.sources[i].enabled) else { return out };
    for s in sessions.iter().filter(|s| !s.cwd.is_empty()) {
        let p = Path::new(&s.cwd).join(sources::aider::FILE);
        if seen.insert(p.clone()) {
            out.extend(sources::common::stat(p).map(|f| Candidate::new(i, Format::Aider, f)));
        }
    }
    out
}

/// Returns (entry, whether a file had to be read) per candidate; `None` = unreadable.
fn parse_parallel(batch: Vec<(&Candidate, Option<Entry>)>, lim: &Limits) -> Vec<(Option<Entry>, bool)> {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
    let per = batch.len().div_ceil(threads).max(1);
    let mut parts: Vec<Vec<(&Candidate, Option<Entry>)>> = Vec::new();
    let mut it = batch.into_iter().peekable();
    while it.peek().is_some() {
        parts.push(it.by_ref().take(per).collect());
    }
    std::thread::scope(|scope| {
        let handles: Vec<_> = parts
            .into_iter()
            .map(|part| {
                let n = part.len();
                (n, scope.spawn(move || part.into_iter().map(|(c, old)| refresh(c, old, lim)).collect::<Vec<_>>()))
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

fn refresh(c: &Candidate, old: Option<Entry>, lim: &Limits) -> (Option<Entry>, bool) {
    // A whole-file session's fingerprint includes its companions. An appended file's
    // size must stay its own: it is compared with the read offset.
    let companions = if c.incremental { Vec::new() } else { sources::companions(c.format, &c.path) };
    let (mut size, mut mtime) = (c.size, c.mtime_ns);
    for p in &companions {
        if let Ok(m) = std::fs::metadata(p) {
            size += m.len();
            mtime = mtime.max(mtime_ns(&m));
        }
    }
    let old = match old {
        Some(e) if e.size == size && e.mtime_ns == mtime => return (Some(e), false),
        // A slim entry has nothing to continue from.
        Some(e) if !e.slim => Some(e),
        _ => None,
    };
    let r = (|| -> std::io::Result<Entry> {
        let fresh = |session| Entry { size, mtime_ns: mtime, head: 0, offset: 0, tail: 0, slim: false, session };
        if !c.incremental {
            let mut session = Session::default();
            sources::parse_file(c.format, &c.path, &companions, &mut session, lim)?;
            return Ok(fresh(session));
        }
        let mut f = File::open(&c.path)?;
        let head = hash_at(&mut f, 0, HASH_BYTES)?;
        // Continue from the old offset only for a pure append: the file grew, and both
        // its first bytes and the bytes up to where we stopped are what we saw before.
        let (mut session, start) = match old {
            Some(e)
                if c.size > e.size
                    && e.head == head
                    && e.offset <= c.size
                    && tail_hash(&mut f, e.offset)? == e.tail =>
            {
                (e.session, e.offset)
            }
            _ => (Session::default(), 0),
        };
        f.seek(SeekFrom::Start(start))?;
        let reader = BufReader::with_capacity(256 * 1024, &mut f);
        let consumed = sources::parse(c.format, &c.path, reader, &mut session, lim)?;
        let offset = start + consumed;
        let tail = tail_hash(&mut f, offset)?;
        Ok(Entry { head, offset, tail, ..fresh(session) })
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
    use crate::sources::common::testing::TempDir;
    use std::io::Write;

    fn test_cfg(root: &Path, cache: &Path) -> Config {
        pool_cfg(root, cache, 10)
    }

    fn pool_cfg(root: &Path, cache: &Path, pool: usize) -> Config {
        let t = format!(
            "[general]\ncache_dir = {cache:?}\nsearch_pool = {pool}\nbuiltin_sources = false\n\
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
        let tmp = TempDir::new("index-append");
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
    }

    /// `n` claude sessions, newest first: `s0` is the newest. `entry(i)` picks each one's
    /// entrypoint (`cli` is interactive, `sdk-cli` headless).
    fn sessions(proj: &Path, n: usize, entry: impl Fn(usize) -> &'static str) {
        std::fs::create_dir_all(proj).unwrap();
        let now = std::time::SystemTime::now();
        for i in 0..n {
            let f = proj.join(format!("s{i}.jsonl"));
            std::fs::write(&f, line(&format!("s{i}"), &format!("prompt {i}"), entry(i))).unwrap();
            let t = now - std::time::Duration::from_secs(i as u64 * 10);
            File::options().write(true).open(&f).unwrap().set_modified(t).unwrap();
        }
    }

    fn cached(cfg: &Config) -> CacheFile {
        read_cache(&cfg.cache_dir().join("index.json"), &cfg.limits())
    }

    #[test]
    fn gone_folders_are_marked() {
        let dir = crate::sources::common::testing::TempDir::new("gone");
        std::fs::create_dir_all(&dir).unwrap();
        let at = |p: &Path| Session { cwd: p.to_string_lossy().into(), ..Default::default() };
        let mut v = vec![at(&dir), at(&dir.join("deleted")), Session::default(), at(&dir.join("deleted"))];
        mark_missing_folders(&mut v);
        assert_eq!(v.iter().map(|s| s.cwd_missing).collect::<Vec<_>>(), [false, true, false, true]);
    }

    #[test]
    fn headless_pile_up_is_kept_slim() {
        let tmp = TempDir::new("index-slim");
        // Ten headless runs, then two interactive sessions older than all of them.
        sessions(&tmp.join("root/projects/-w"), 12, |i| if i < 10 { "sdk-cli" } else { "cli" });
        let cfg = pool_cfg(&tmp.join("root"), &tmp.join("cache"), 3);
        let l = load(&cfg, false).unwrap();
        let ids: Vec<&str> = l.sessions.iter().map(|s| s.id.as_str()).collect();
        // The newest 3 of the source (for Ctrl+A) and the interactive ones it lists.
        assert_eq!(ids, vec!["s0", "s1", "s2", "s10", "s11"]);
        let c = cached(&cfg);
        assert_eq!(c.entries.len(), 12, "every file read is remembered...");
        let slim: Vec<&Entry> = c.entries.values().filter(|e| e.slim).collect();
        assert_eq!(slim.len(), 7, "...but those that can not be shown only as a marker");
        assert!(slim.iter().all(|e| e.session.prompts.is_empty() && e.session.entry.as_deref() == Some("sdk-cli")));

        // A bigger pool wants the slim ones after all: they are read again, in full.
        let cfg = pool_cfg(&tmp.join("root"), &tmp.join("cache"), 20);
        let l = load(&cfg, false).unwrap();
        assert_eq!(l.sessions.len(), 12);
        assert!(l.sessions.iter().all(|s| s.prompts.len() == 1));
        assert!(cached(&cfg).entries.values().all(|e| !e.slim));
    }

    #[test]
    fn files_older_than_a_full_source_are_not_read() {
        let tmp = TempDir::new("index-full");
        sessions(&tmp.join("root/projects/-w"), BATCH + 12, |_| "cli");
        let cfg = pool_cfg(&tmp.join("root"), &tmp.join("cache"), 2);
        let l = load(&cfg, false).unwrap();
        assert_eq!(l.sessions.len(), 2);
        // The first batch is read whole; once the source is full the rest is skipped.
        assert_eq!(cached(&cfg).entries.len(), BATCH);
    }

    #[test]
    fn companion_change_invalidates_whole_file_entry() {
        let tmp = TempDir::new("index-companion");
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
    }
}
