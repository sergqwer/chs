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
    /// Prompt that matched best, for the preview snippet.
    pub prompt: Option<usize>,
}

pub struct Searcher {
    matcher: Matcher,
    buf: Vec<char>,
    buf2: Vec<char>,
    cfg: SearchCfg,
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

impl Searcher {
    pub fn new(cfg: &SearchCfg) -> Searcher {
        Searcher { matcher: Matcher::new(MatcherConfig::DEFAULT), buf: Vec::new(), buf2: Vec::new(), cfg: cfg.clone() }
    }

    pub fn parse(&mut self, query: &str) -> Query {
        let case = match self.cfg.case {
            CaseMode::Smart => CaseMatching::Smart,
            CaseMode::Ignore => CaseMatching::Ignore,
            CaseMode::Respect => CaseMatching::Respect,
        };
        let norm = if self.cfg.normalize { Normalization::Smart } else { Normalization::Never };
        let atoms = Pattern::parse(query, case, norm).atoms;
        let self_scores =
            atoms.iter().map(|a| a.score(a.needle_text(), &mut self.matcher).unwrap_or(1).max(1) as f64).collect();
        let needle_lens = atoms.iter().map(|a| a.needle_text().len()).collect();
        Query { atoms, self_scores, needle_lens }
    }

    /// Quality (0-1) of `atom` against `text`, if it clears the threshold and, for a
    /// fuzzy atom, the match is compact enough to sit inside about one word.
    fn quality(&mut self, q: &Query, i: usize, text: &str) -> Option<f64> {
        let atom = &q.atoms[i];
        let hay = Utf32Str::new(text, &mut self.buf);
        let s = atom.score(hay, &mut self.matcher)? as f64;
        let quality = (s / q.self_scores[i]).min(1.0);
        if quality < self.cfg.min_quality {
            return None;
        }
        if atom.kind == AtomKind::Fuzzy {
            let mut idx = Vec::new();
            atom.indices(Utf32Str::new(text, &mut self.buf), &mut self.matcher, &mut idx)?;
            let (lo, hi) = (*idx.iter().min()?, *idx.iter().max()?);
            let spread = (hi - lo + 1) as f64;
            if spread > q.needle_lens[i] as f64 * self.cfg.max_spread + 2.0 {
                return None;
            }
        }
        Some(quality)
    }

    /// Rank `pool` (newest first): each session with its display title.
    pub fn search(&mut self, q: &Query, pool: &[(&Session, &str)], limit: usize) -> Vec<Hit> {
        if q.is_empty() {
            return Vec::new();
        }
        let (wt, wf, wp) = (self.cfg.weight_title, self.cfg.weight_folder, self.cfg.weight_prompts);
        let wmax = wt.max(wf).max(wp).max(f64::EPSILON);
        let n = pool.len().max(1) as f64;
        let mut hits = Vec::new();
        'sessions: for (idx, &(s, title)) in pool.iter().enumerate() {
            let mut total = 0.0;
            let mut best_prompt: Option<(usize, f64)> = None;
            for i in 0..q.atoms.len() {
                if q.atoms[i].negative {
                    // Excluded words must be absent from every field.
                    let atom = &q.atoms[i];
                    let fields = std::iter::once(title)
                        .chain(std::iter::once(s.cwd.as_str()))
                        .chain(s.prompts.iter().map(String::as_str));
                    for f in fields {
                        if atom.score(Utf32Str::new(f, &mut self.buf), &mut self.matcher).is_none() {
                            continue 'sessions;
                        }
                    }
                    continue;
                }
                let mut best = 0.0f64;
                if let Some(x) = self.quality(q, i, title) {
                    best = best.max(x * wt);
                }
                if let Some(x) = self.quality(q, i, &s.cwd) {
                    best = best.max(x * wf);
                }
                for (pi, p) in s.prompts.iter().enumerate() {
                    if let Some(x) = self.quality(q, i, p) {
                        if best_prompt.is_none_or(|(_, b)| x > b) {
                            best_prompt = Some((pi, x));
                        }
                        best = best.max(x * wp);
                    }
                }
                if best <= 0.0 {
                    continue 'sessions;
                }
                total += best;
            }
            let positive = q.atoms.iter().filter(|a| !a.negative).count().max(1) as f64;
            let recency = 1.0 - idx as f64 / n;
            let score = total / positive / wmax + self.cfg.recency_weight * recency;
            hits.push(Hit { idx, score, prompt: best_prompt.map(|(i, _)| i) });
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.idx.cmp(&b.idx)));
        hits.truncate(limit);
        hits
    }

    /// Char positions in `text` to highlight for the query.
    pub fn highlight(&mut self, q: &Query, text: &str) -> Vec<usize> {
        let mut out: Vec<u32> = Vec::new();
        for (i, atom) in q.atoms.iter().enumerate() {
            if atom.negative {
                continue;
            }
            if self.quality(q, i, text).is_none() {
                continue;
            }
            let mut idx = Vec::new();
            if atom.indices(Utf32Str::new(text, &mut self.buf2), &mut self.matcher, &mut idx).is_some() {
                out.extend(idx);
            }
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
    fn negation_excludes() {
        let r = run("proxy !nginx", &pool());
        assert_eq!(r, vec!["proxy pool health"]);
    }
}
