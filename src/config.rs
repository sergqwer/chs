//! Configuration.
//!
//! The commented template `config.default.toml` is embedded in the binary and is the
//! single source of default values: the user's file is deep-merged on top of it, so a
//! key missing from the user's file simply keeps its template value and the template
//! can never drift from what the code expects.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const DEFAULT_CONFIG: &str = include_str!("../config.default.toml");

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub general: General,
    pub display: Display,
    pub search: Search,
    pub theme: Theme,
    pub keys: Keys,
    pub linux: Linux,
    pub windows: Windows,
    pub sources: Vec<SourceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct General {
    pub recent_count: usize,
    pub search_pool: usize,
    pub result_count: usize,
    pub language_file: String,
    pub cache_dir: String,
    pub after_resume: AfterResume,
    pub missing_cwd: MissingCwd,
    pub esc_clears_query: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AfterResume {
    Exit,
    Return,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MissingCwd {
    Home,
    Current,
    Abort,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Display {
    pub columns: Vec<Column>,
    pub date_format: String,
    pub relative_dates: bool,
    pub folder_style: FolderStyle,
    pub folder_width: u16,
    pub source_width: u16,
    pub show_headless_tag: bool,
    pub headless_tag: String,
    pub highlight_symbol: String,
    pub border: Border,
    pub preview: bool,
    pub preview_position: PreviewPosition,
    pub preview_auto_width: u16,
    pub preview_size: u16,
    pub preview_first_prompts: usize,
    pub preview_last_prompts: usize,
    pub preview_prompt_chars: usize,
    pub show_help: bool,
    pub height: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Column {
    Source,
    Date,
    Folder,
    Title,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FolderStyle {
    Name,
    Tilde,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Border {
    Rounded,
    Plain,
    Double,
    Thick,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreviewPosition {
    Right,
    Bottom,
    Auto,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Search {
    pub weight_title: f64,
    pub weight_folder: f64,
    pub weight_prompts: f64,
    pub case: CaseMode,
    pub normalize: bool,
    pub min_quality: f64,
    pub max_spread: f64,
    pub recency_weight: f64,
    pub max_indexed_chars: usize,
    pub max_prompt_chars: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaseMode {
    Smart,
    Ignore,
    Respect,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub selected_bg: String,
    pub selected_fg: String,
    pub match_fg: String,
    pub date_fg: String,
    pub folder_fg: String,
    pub title_fg: String,
    pub headless_fg: String,
    pub border_fg: String,
    pub query_fg: String,
    pub prompt_symbol: String,
    pub help_fg: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keys {
    pub up: Vec<String>,
    pub down: Vec<String>,
    pub page_up: Vec<String>,
    pub page_down: Vec<String>,
    pub first: Vec<String>,
    pub last: Vec<String>,
    pub resume: Vec<String>,
    pub print_command: Vec<String>,
    pub quit: Vec<String>,
    pub clear_query: Vec<String>,
    pub delete_word: Vec<String>,
    pub toggle_preview: Vec<String>,
    pub toggle_headless: Vec<String>,
    pub next_source: Vec<String>,
    pub prev_source: Vec<String>,
    pub reindex: Vec<String>,
    pub cursor_left: Vec<String>,
    pub cursor_right: Vec<String>,
    pub cursor_home: Vec<String>,
    pub cursor_end: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Linux {
    pub add_to_path: bool,
    pub link_dir: String,
    pub use_sudo: bool,
    pub fallback_dir: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Windows {
    pub add_to_path: bool,
    pub tray_enabled: bool,
    pub tray_autostart: bool,
    pub start_tray_with_cli: bool,
    pub terminal: Terminal,
    pub terminal_command: Vec<String>,
    pub tray_tooltip: String,
    pub tray_left_click: TrayClick,
    pub editor: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Terminal {
    Auto,
    Wt,
    Conhost,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrayClick {
    Open,
    Menu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Claude,
    Codex,
    Opencode,
    Crush,
    Goose,
}

/// One folder or several candidates; every one that exists is read. Candidates
/// using an environment variable that is not set are skipped, so one list can cover
/// Linux, macOS and Windows locations and the tools' own override variables.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Paths {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    pub name: String,
    pub format: Format,
    pub path: Paths,
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub include_headless: bool,
    #[serde(default)]
    pub headless_markers: Option<Vec<String>>,
    pub resume_command: Vec<String>,
}

fn default_color() -> String {
    "cyan".into()
}
fn yes() -> bool {
    true
}

impl SourceConfig {
    /// `entrypoint` (claude) / `source` (codex) values that mark a session as headless.
    pub fn headless_markers(&self) -> Vec<String> {
        if let Some(m) = &self.headless_markers {
            return m.clone();
        }
        match self.format {
            // `cli` is the interactive TUI; everything SDK-driven (`claude -p`, the agent
            // SDKs, cllm-style wrappers) reports an sdk-* entrypoint.
            Format::Claude => vec!["sdk-cli".into(), "sdk-ts".into(), "sdk-py".into()],
            // `exec` is `codex exec`; `subagent` is what a JSON-object source maps to.
            Format::Codex => vec!["exec".into(), "subagent".into(), "mcp".into()],
            // Child sessions spawned by a task/subagent tool.
            Format::Opencode | Format::Crush => vec!["subagent".into()],
            // Goose's session_type: only "user" (and the "terminal" integration) is a
            // conversation you had; the rest is scheduled, spawned or other clients.
            Format::Goose => {
                vec!["scheduled".into(), "sub_agent".into(), "hidden".into(), "gateway".into(), "acp".into()]
            }
        }
    }

    /// Existing folders among `path`, without duplicates.
    pub fn roots(&self) -> Vec<PathBuf> {
        let list = match &self.path {
            Paths::One(p) => std::slice::from_ref(p).to_vec(),
            Paths::Many(v) => v.clone(),
        };
        let mut seen = std::collections::HashSet::new();
        list.iter()
            .filter_map(|p| try_expand_path(p))
            .filter(|p| p.is_dir())
            .filter(|p| seen.insert(p.canonicalize().unwrap_or_else(|_| p.clone())))
            .collect()
    }
}

impl Config {
    pub fn default_config() -> Config {
        toml::from_str(DEFAULT_CONFIG).expect("embedded default config is valid")
    }

    /// Parse a user config, deep-merged over the embedded defaults.
    pub fn from_user_str(user: &str) -> Result<Config> {
        let mut base: toml::Table = toml::from_str(DEFAULT_CONFIG).expect("embedded default config");
        let over: toml::Table = toml::from_str(user).context("config is not valid TOML")?;
        merge(&mut base, over);
        let cfg: Config = toml::Value::Table(base).try_into().context("config has an invalid value")?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for s in &self.sources {
            if s.name.trim().is_empty() {
                bail!("a [[sources]] entry has an empty name");
            }
            if !seen.insert(s.name.clone()) {
                bail!("two [[sources]] entries are both named {:?}", s.name);
            }
            if s.resume_command.is_empty() {
                bail!("source {:?}: resume_command is empty", s.name);
            }
        }
        if self.general.search_pool == 0 {
            bail!("general.search_pool must be at least 1");
        }
        Ok(())
    }

    pub fn source(&self, name: &str) -> Option<&SourceConfig> {
        self.sources.iter().find(|s| s.name == name)
    }

    pub fn cache_dir(&self) -> PathBuf {
        if !self.general.cache_dir.trim().is_empty() {
            return expand_path(&self.general.cache_dir);
        }
        cache_root()
    }
}

/// Tables merge key by key; everything else (arrays included) is replaced.
fn merge(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

/// `CHS_CONFIG` overrides; otherwise `~/.config/chs/config.toml` (Linux),
/// `%APPDATA%\chs\config.toml` (Windows), `~/Library/Application Support/chs` (macOS).
pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("CHS_CONFIG") {
        return PathBuf::from(p);
    }
    let base = if cfg!(target_os = "macos") { dirs::home_dir().map(|h| h.join(".config")) } else { dirs::config_dir() };
    base.unwrap_or_else(|| PathBuf::from(".")).join("chs").join("config.toml")
}

pub fn cache_root() -> PathBuf {
    if cfg!(windows) { dirs::data_local_dir() } else { dirs::cache_dir() }
        .unwrap_or_else(std::env::temp_dir)
        .join("chs")
}

/// Load the config, creating it from the template on first run.
/// Returns the config and whether the file was just created.
pub fn load() -> Result<(Config, bool)> {
    let path = config_path();
    let mut created = false;
    if !path.exists() {
        write_default(&path, false)?;
        created = true;
    }
    let text = std::fs::read_to_string(&path).with_context(|| format!("can not read {}", path.display()))?;
    let cfg = Config::from_user_str(&text).with_context(|| format!("in {}", path.display()))?;
    Ok((cfg, created))
}

pub fn write_default(path: &Path, force: bool) -> Result<bool> {
    if path.exists() && !force {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("can not create {}", dir.display()))?;
    }
    let text = if cfg!(windows) { DEFAULT_CONFIG.replace('\n', "\r\n") } else { DEFAULT_CONFIG.to_string() };
    std::fs::write(path, text).with_context(|| format!("can not write {}", path.display()))?;
    Ok(true)
}

/// Set `key = true|false` inside `[section]` of a config text, keeping every comment
/// and the rest of the file untouched (used by the tray's "Start with Windows").
pub fn set_bool(text: &str, section: &str, key: &str, value: bool) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let header = format!("[{section}]");
    let new_line = format!("{key} = {value}");
    // A header may carry a comment: `[windows]  # Windows settings`.
    let is_header = |l: &str| l.split('#').next().unwrap_or("").trim() == header;
    let Some(start) = lines.iter().position(|l| is_header(l)) else {
        let mut out = text.trim_end().to_string();
        out.push_str(&format!("{nl}{nl}{header}{nl}{new_line}{nl}"));
        return out;
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .map(|i| start + 1 + i)
        .unwrap_or(lines.len());
    let is_key = |l: &str| {
        let l = l.trim_start();
        l.strip_prefix(key).is_some_and(|r| r.trim_start().starts_with('='))
    };
    match (start + 1..end).find(|&i| is_key(&lines[i])) {
        Some(i) => {
            let indent: String = lines[i].chars().take_while(|c| c.is_whitespace()).collect();
            let comment =
                lines[i].find('#').map(|p| lines[i][lines[i][..p].trim_end().len()..].to_string()).unwrap_or_default();
            lines[i] = format!("{indent}{new_line}{comment}");
        }
        None => lines.insert(start + 1, new_line),
    }
    let mut out = lines.join(nl);
    out.push_str(nl);
    out
}

/// Expand `~`, `${VAR}` and `%VAR%`; unset variables are left as written.
pub fn expand_path(p: &str) -> PathBuf {
    expand_inner(p).0
}

/// Like [`expand_path`], but `None` when the path uses a variable that is not set.
pub fn try_expand_path(p: &str) -> Option<PathBuf> {
    let (path, complete) = expand_inner(p);
    complete.then_some(path)
}

fn expand_inner(p: &str) -> (PathBuf, bool) {
    let (p, complete) = expand_vars(p.trim());
    (expand_tilde(&p), complete)
}

fn expand_tilde(p: &str) -> PathBuf {
    if p == "~" {
        return dirs::home_dir().unwrap_or_default();
    }
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        return dirs::home_dir().unwrap_or_default().join(rest);
    }
    PathBuf::from(p)
}

/// Returns the expanded text and whether every variable was set (and non-empty).
fn expand_vars(s: &str) -> (String, bool) {
    let mut complete = true;
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let dollar = rest.find("${");
        let pct = rest.find('%');
        let next = match (dollar, pct) {
            (Some(d), Some(p)) => Some(d.min(p)),
            (a, b) => a.or(b),
        };
        let Some(i) = next else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let (name, consumed) = if let Some(t) = tail.strip_prefix("${") {
            match t.find('}') {
                Some(e) => (Some(&t[..e]), e + 3),
                None => (None, 2),
            }
        } else {
            let t = &tail[1..];
            match t.find('%') {
                Some(e) if e > 0 && t[..e].chars().all(|c| c.is_alphanumeric() || c == '_') => (Some(&t[..e]), e + 2),
                _ => (None, 1),
            }
        };
        match name.map(|n| std::env::var(n).ok().filter(|v| !v.is_empty())) {
            Some(Some(v)) => out.push_str(&v),
            Some(None) => {
                complete = false;
                out.push_str(&tail[..consumed]);
            }
            None => out.push_str(&tail[..consumed]),
        }
        rest = &tail[consumed..];
    }
    (out, complete)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses() {
        let c = Config::from_user_str("").unwrap();
        assert_eq!(c.general.recent_count, 20);
        assert_eq!(c.general.search_pool, 200);
        assert!(c.sources.len() >= 4);
    }

    #[test]
    fn partial_override_keeps_defaults() {
        let c = Config::from_user_str("[general]\nrecent_count = 5\n[display]\npreview = false\n").unwrap();
        assert_eq!(c.general.recent_count, 5);
        assert_eq!(c.general.result_count, 20);
        assert!(!c.display.preview);
        assert_eq!(c.display.date_format, "%Y-%m-%d %H:%M");
    }

    #[test]
    fn sources_replace_wholesale() {
        let c = Config::from_user_str(
            "[[sources]]\nname='x'\nformat='codex'\npath='/x'\nresume_command=['codex','resume','{id}']\n",
        )
        .unwrap();
        assert_eq!(c.sources.len(), 1);
        assert!(c.sources[0].enabled);
        assert_eq!(c.sources[0].headless_markers(), vec!["exec", "subagent", "mcp"]);
    }

    #[test]
    fn unknown_key_is_an_error() {
        assert!(Config::from_user_str("[general]\nrecnt_count = 5\n").is_err());
    }

    #[test]
    fn set_bool_keeps_comments() {
        let t = "# top\n[windows]\n# comment\ntray_autostart = true  # inline\nother = 1\n[x]\ntray_autostart = true\n";
        let out = set_bool(t, "windows", "tray_autostart", false);
        assert_eq!(
            out,
            "# top\n[windows]\n# comment\ntray_autostart = false  # inline\nother = 1\n[x]\ntray_autostart = true\n"
        );
        let out = set_bool("[windows] # mine\ntray_autostart = true\n", "windows", "tray_autostart", false);
        assert_eq!(out, "[windows] # mine\ntray_autostart = false\n");
        let out = set_bool("[general]\n", "windows", "tray_autostart", true);
        assert!(Config::from_user_str(&out).unwrap().windows.tray_autostart);
        let full = set_bool(DEFAULT_CONFIG, "windows", "tray_autostart", false);
        assert!(!Config::from_user_str(&full).unwrap().windows.tray_autostart);
    }

    #[test]
    fn vars_expand() {
        std::env::set_var("CHS_TEST_VAR", "abc");
        assert_eq!(expand_vars("${CHS_TEST_VAR}/x"), ("abc/x".into(), true));
        assert_eq!(expand_vars("%CHS_TEST_VAR%\\x"), ("abc\\x".into(), true));
        assert_eq!(expand_vars("100% sure"), ("100% sure".into(), true));
        assert_eq!(expand_vars("${NOPE_NOT_SET_42}"), ("${NOPE_NOT_SET_42}".into(), false));
        assert!(try_expand_path("${NOPE_NOT_SET_42}/goose").is_none());
        assert!(try_expand_path("~/x").is_some());
    }
}
