//! One conversation, whatever tool it came from.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How much prompt text is kept per session.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_prompt_chars: usize,
    pub max_indexed_chars: usize,
}

#[cfg(test)]
impl Limits {
    /// The default budget, for tests.
    pub const TEST: Limits = Limits { max_prompt_chars: 4000, max_indexed_chars: 40000 };
}

/// When a session exceeds `max_indexed_chars`, this many opening prompts are always
/// kept; the rest of the budget goes to the most recent ones.
const KEEP_HEAD: usize = 5;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    pub source: String,
    pub id: String,
    pub file: PathBuf,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub started_ms: i64,
    /// File modification time; the list is sorted by it.
    pub updated_ms: i64,
    /// `entrypoint` (claude) or `source` (codex): decides "headless".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub prompts: Vec<String>,
    /// Codex only: prompts recovered from raw response items, used when the file
    /// carries no explicit user-message events (older/other writers).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alt_prompts: Vec<String>,
    #[serde(default)]
    pub alt_chars: usize,
    pub prompt_count: u32,
    pub prompt_chars: usize,
    /// Computed from `entry` and the source's config every load; never cached.
    #[serde(skip)]
    pub headless: bool,
    /// Index of the source in `Config::sources`, set on every load.
    #[serde(skip)]
    pub src: usize,
    /// `prompts` as the matcher reads them, made by the first search that reaches
    /// this session. Lives with the prompts, so a reloaded list starts without it.
    #[serde(skip)]
    pub decoded: std::sync::OnceLock<Box<[crate::search::Decoded]>>,
}

impl Session {
    pub fn title(&self) -> String {
        for t in [&self.custom_title, &self.ai_title, &self.summary].into_iter().flatten() {
            let t = one_line(t);
            if !t.is_empty() {
                return t;
            }
        }
        self.prompts.first().map(|p| one_line(p)).unwrap_or_default()
    }

    /// Something worth listing: a resumable id and at least a prompt or a title.
    pub fn is_useful(&self) -> bool {
        !self.id.is_empty() && self.has_content()
    }

    pub fn has_content(&self) -> bool {
        !self.prompts.is_empty() || self.custom_title.is_some() || self.ai_title.is_some()
    }

    pub fn push_prompt(&mut self, text: &str, lim: &Limits) {
        if push_capped(&mut self.prompts, &mut self.prompt_chars, text, lim) {
            self.prompt_count += 1;
        }
    }

    /// Codex fallback prompts, under the same budget as the real ones.
    pub fn push_alt_prompt(&mut self, text: &str, lim: &Limits) {
        push_capped(&mut self.alt_prompts, &mut self.alt_chars, text, lim);
    }
}

/// Append a prompt, keeping the first KEEP_HEAD and the latest ones within the
/// character budget. Returns false for an empty prompt or a repeat of the last one.
fn push_capped(list: &mut Vec<String>, chars: &mut usize, text: &str, lim: &Limits) -> bool {
    let text = truncate_chars(text.trim(), lim.max_prompt_chars);
    if text.is_empty() || list.last().is_some_and(|l| *l == text) {
        return false;
    }
    *chars += text.chars().count();
    list.push(text);
    while *chars > lim.max_indexed_chars && list.len() > KEEP_HEAD + 1 {
        let gone = list.remove(KEEP_HEAD);
        *chars -= gone.chars().count();
    }
    true
}

/// Collapse all whitespace (newlines included) into single spaces.
pub fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(512));
    for w in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
        if out.len() > 1000 {
            break;
        }
    }
    out
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

pub fn parse_ts(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts).ok().map(|d| d.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIM: Limits = Limits { max_prompt_chars: 10, max_indexed_chars: 40 };

    #[test]
    fn prompts_are_capped_keeping_head_and_tail() {
        let mut s = Session::default();
        for i in 0..20 {
            s.push_prompt(&format!("p{i:02}"), &LIM);
        }
        assert_eq!(s.prompt_count, 20);
        assert!(s.prompt_chars <= 40);
        assert_eq!(&s.prompts[..KEEP_HEAD], &["p00", "p01", "p02", "p03", "p04"]);
        assert_eq!(s.prompts.last().unwrap(), "p19");
    }

    #[test]
    fn long_prompt_is_cut_and_dupes_skipped() {
        let mut s = Session::default();
        s.push_prompt("abcdefghijklmnop", &LIM);
        s.push_prompt("abcdefghijklmnop", &LIM);
        assert_eq!(s.prompts, vec!["abcdefghij…"]);
    }

    #[test]
    fn title_priority() {
        let mut s = Session::default();
        s.prompts.push("first\nprompt".into());
        assert_eq!(s.title(), "first prompt");
        s.ai_title = Some("AI".into());
        assert_eq!(s.title(), "AI");
        s.custom_title = Some("Mine".into());
        assert_eq!(s.title(), "Mine");
    }
}
