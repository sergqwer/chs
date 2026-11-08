//! Fuzzy ranking over title, folder and every prompt of each session.
//!
//! The query is split into fzf-style atoms (words are ANDed; 'exact ^prefix suffix$
//! !negate). Each atom may be satisfied by a different field: "proxy rust" finds a
//! session titled "proxy pool" whose prompts mention rust. A field's score is divided
//! by the atom's score against itself, which turns nucleo's raw score into a 0-1
//! quality that means the same for a 3-letter and a 12-letter word, so one threshold
//! (`min_quality`) can drop weak scattered matches in long prompts.

use crate::config::{CaseMode, Search as SearchCfg};
use crate::session::Session;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

pub struct Hit {
    /// Index into the slice that was searched.
    pub idx: usize,
    pub score: f64,
}

pub struct Searcher {
    sc: Scorer,
    /// Decoding buffer for the short fields (title, folder) and highlighting.
    buf: Vec<char>,
}

/// The matcher and what scoring one field against one atom needs.
struct Scorer {
    matcher: Matcher,
    /// Match positions, reused between calls.
    idx: Vec<u32>,
    cfg: SearchCfg,
}

/// A prompt as nucleo reads it. ASCII is matched in place; anything else becomes one
/// char per grapheme, which costs more than the match itself, so it is done once per
/// prompt (on the first search that reaches the session) instead of on every key.
#[derive(Debug, Clone)]
pub struct Decoded(Option<Box<[char]>>);

impl Decoded {
    fn new(text: &str, buf: &mut Vec<char>) -> Decoded {
        // The matcher's own conversion, so the result is exactly what it would match.
        match Utf32Str::new(text, buf) {
            Utf32Str::Ascii(_) => Decoded(None),
            Utf32Str::Unicode(c) => Decoded(Some(c.into())),
        }
    }

    fn hay<'a>(&'a self, text: &'a str) -> Utf32Str<'a> {
        match &self.0 {
            None => Utf32Str::Ascii(text.as_bytes()),
            Some(c) => Utf32Str::Unicode(c),
        }
    }
}

/// The session's prompts, each paired with its decoded form.
fn prompts<'s>(s: &'s Session, buf: &mut Vec<char>) -> impl Iterator<Item = Utf32Str<'s>> {
    let decoded = s.decoded.get_or_init(|| s.prompts.iter().map(|p| Decoded::new(p, buf)).collect());
    s.prompts.iter().zip(decoded.iter()).map(|(p, d)| d.hay(p))
}

/// Parsed query, reused for highlighting the rows on screen.
pub struct Query {
    atoms: Vec<Atom>,
    self_scores: Vec<f64>,
    needle_lens: Vec<usize>,
}

impl Query {
    pub fn is_empty(&self) -> bool {
        self.atoms.is_empty()
    }
}

impl Scorer {
    /// Quality (0-1) of atom `i` against `hay`, if it clears the threshold and, for a
    /// fuzzy atom, the match is compact enough to sit inside about one word.
    fn quality(&mut self, q: &Query, i: usize, hay: Utf32Str) -> Option<f64> {
        let atom = &q.atoms[i];
        let s = atom.score(hay, &mut self.matcher)? as f64;
        let quality = (s / q.self_scores[i]).min(1.0);
        if quality < self.cfg.min_quality {
            return None;
        }
        if atom.kind == AtomKind::Fuzzy {
            let idx = &mut self.idx;
            idx.clear();
            atom.indices(hay, &mut self.matcher, idx)?;
            let (lo, hi) = (*idx.iter().min()?, *idx.iter().max()?);
            let spread = (hi - lo + 1) as f64;
            if spread > q.needle_lens[i] as f64 * self.cfg.max_spread + 2.0 {
                return None;
            }
        }
        Some(quality)
    }
}

impl Searcher {
    pub fn new(cfg: &SearchCfg) -> Searcher {
        Searcher {
            sc: Scorer { matcher: Matcher::new(MatcherConfig::DEFAULT), idx: Vec::new(), cfg: cfg.clone() },
            buf: Vec::new(),
        }
    }

    pub fn parse(&mut self, query: &str) -> Query {
        let cfg = &self.sc.cfg;
        let case = match cfg.case {
            CaseMode::Smart => CaseMatching::Smart,
            CaseMode::Ignore => CaseMatching::Ignore,
            CaseMode::Respect => CaseMatching::Respect,
        };
        let norm = if cfg.normalize { Normalization::Smart } else { Normalization::Never };
        let atoms = Pattern::parse(query, case, norm).atoms;
        let m = &mut self.sc.matcher;
        let self_scores = atoms.iter().map(|a| a.score(a.needle_text(), m).unwrap_or(1).max(1) as f64).collect();
        let needle_lens = atoms.iter().map(|a| a.needle_text().len()).collect();
        Query { atoms, self_scores, needle_lens }
    }

    /// Rank `pool` (newest first): each session with its display title.
    pub fn search(&mut self, q: &Query, pool: &[(&Session, &str)], limit: usize) -> Vec<Hit> {
        if q.is_empty() {
            return Vec::new();
        }
        let cfg = &self.sc.cfg;
        let (wt, wf, wp) = (cfg.weight_title, cfg.weight_folder, cfg.weight_prompts);
        let wmax = wt.max(wf).max(wp).max(f64::EPSILON);
        let recency_weight = cfg.recency_weight;
        let positive = q.atoms.iter().filter(|a| !a.negative).count().max(1) as f64;
        let n = pool.len().max(1) as f64;
        let mut hits = Vec::new();
        'sessions: for (idx, &(s, title)) in pool.iter().enumerate() {
            let mut total = 0.0;
            for i in 0..q.atoms.len() {
                let atom = &q.atoms[i];
                if atom.negative {
                    // Excluded words must be absent from every field.
                    let m = &mut self.sc.matcher;
                    if atom.score(Utf32Str::new(title, &mut self.buf), m).is_none()
                        || atom.score(Utf32Str::new(&s.cwd, &mut self.buf), m).is_none()
                        || prompts(s, &mut self.buf).any(|p| atom.score(p, m).is_none())
                    {
                        continue 'sessions;
                    }
                    continue;
                }
                let mut best = 0.0f64;
                if let Some(x) = self.sc.quality(q, i, Utf32Str::new(title, &mut self.buf)) {
                    best = best.max(x * wt);
                }
                if let Some(x) = self.sc.quality(q, i, Utf32Str::new(&s.cwd, &mut self.buf)) {
                    best = best.max(x * wf);
                }
                // A prompt adds at most `wp`: when the title or folder already beat
                // that, the prompts (most of the text) can not change the score.
                if best < wp {
                    best = best.max(self.scan(q, i, s, &mut None) * wp);
                }
                if best <= 0.0 {
                    continue 'sessions;
                }
                total += best;
            }
            let recency = 1.0 - idx as f64 / n;
            let score = total / positive / wmax + recency_weight * recency;
            hits.push(Hit { idx, score });
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.idx.cmp(&b.idx)));
        hits.truncate(limit);
        hits
    }

    /// The prompt that matches the query best, for the preview of the selected row:
    /// asked for one session at a time, so the ranking need not track it for all.
    pub fn best_prompt(&mut self, q: &Query, s: &Session) -> Option<usize> {
        let mut top = None;
        for i in (0..q.atoms.len()).filter(|&i| !q.atoms[i].negative) {
            self.scan(q, i, s, &mut top);
        }
        top.map(|(pi, _)| pi)
    }

    /// Best quality of atom `i` in the prompts (0 if none matches); the best prompt
    /// for any atom so far is kept in `top`.
    fn scan(&mut self, q: &Query, i: usize, s: &Session, top: &mut Option<(usize, f64)>) -> f64 {
        let mut best = 0.0f64;
        for (pi, p) in prompts(s, &mut self.buf).enumerate() {
            if let Some(x) = self.sc.quality(q, i, p) {
                if top.is_none_or(|(_, b)| x > b) {
                    *top = Some((pi, x));
                }
                best = best.max(x);
            }
        }
        best
    }

    /// Char positions in `text` to highlight for the query.
    pub fn highlight(&mut self, q: &Query, text: &str) -> Vec<usize> {
        let hay = Utf32Str::new(text, &mut self.buf);
        let mut out: Vec<u32> = Vec::new();
        for (i, atom) in q.atoms.iter().enumerate() {
            if atom.negative || self.sc.quality(q, i, hay).is_none() {
                continue;
            }
            atom.indices(hay, &mut self.sc.matcher, &mut out);
        }
        out.sort_unstable();
        out.dedup();
        out.into_iter().map(|x| x as usize).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn sess(title: &str, cwd: &str, prompts: &[&str]) -> Session {
        Session {
            id: title.into(),
            cwd: cwd.into(),
            custom_title: Some(title.into()),
            prompts: prompts.iter().map(|p| p.to_string()).collect(),
            ..Default::default()
        }
    }

    fn run(q: &str, pool: &[Session]) -> Vec<String> {
        let cfg = Config::default_config();
        let mut s = Searcher::new(&cfg.search);
        let titles: Vec<String> = pool.iter().map(|s| s.title()).collect();
        let refs: Vec<(&Session, &str)> = pool.iter().zip(&titles).map(|(s, t)| (s, t.as_str())).collect();
        let query = s.parse(q);
        s.search(&query, &refs, 20).into_iter().map(|h| pool[h.idx].id.clone()).collect()
    }

    fn pool() -> Vec<Session> {
        vec![
            sess("nginx config", "/home/u/nginx-conf", &["set up reverse proxy for the site"]),
            sess("proxy pool health", "/home/u/socks-pool", &["check why socks5 pool is slow"]),
            sess("rust checker rewrite", "/home/u/checker", &["Потрібно переробити чекер на rust"]),
            sess("unrelated", "/tmp", &["please run our extra yaml checks, then report on the box"]),
        ]
    }

    #[test]
    fn title_beats_prompt() {
        let r = run("proxy", &pool());
        assert_eq!(r[0], "proxy pool health");
        assert!(r.contains(&"nginx config".to_string()));
        assert!(!r.contains(&"unrelated".to_string()), "scattered letters must not match: {r:?}");
    }

    #[test]
    fn words_can_match_different_fields() {
        assert_eq!(run("checker переробити", &pool()), vec!["rust checker rewrite"]);
    }

    #[test]
    fn cyrillic_typo_tolerant() {
        // a letter missing from a word still finds it
        assert_eq!(run("перерб чекр", &pool()), vec!["rust checker rewrite"]);
    }

    #[test]
    fn best_prompt_found_when_the_title_decides() {
        // The title outweighs any prompt, so ranking skips the prompts; the preview
        // still gets the one that matches.
        let pool = pool();
        let mut s = Searcher::new(&Config::default_config().search);
        let q = s.parse("pool");
        assert_eq!(s.best_prompt(&q, &pool[1]), Some(0));
        assert_eq!(s.best_prompt(&q, &pool[3]), None);
    }

    #[test]
    fn decoded_prompts_are_reused() {
        let pool = pool();
        let titles: Vec<String> = pool.iter().map(|s| s.title()).collect();
        let refs: Vec<(&Session, &str)> = pool.iter().zip(&titles).map(|(s, t)| (s, t.as_str())).collect();
        let mut s = Searcher::new(&Config::default_config().search);
        let q = s.parse("переробити");
        let first: Vec<(usize, f64)> = s.search(&q, &refs, 20).iter().map(|h| (h.idx, h.score)).collect();
        assert!(pool[2].decoded.get().is_some_and(|d| d.len() == 1), "Cyrillic prompt decoded once");
        let again: Vec<(usize, f64)> = s.search(&q, &refs, 20).iter().map(|h| (h.idx, h.score)).collect();
        assert_eq!(first, again);
        assert_eq!(first.first().map(|h| h.0), Some(2));
    }

    #[test]
    fn negation_excludes() {
        let r = run("proxy !nginx", &pool());
        assert_eq!(r, vec!["proxy pool health"]);
    }
}
