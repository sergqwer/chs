//! UI strings. English is built in; `general.language_file` points at a TOML file that
//! overrides any subset of them at startup, so a translation needs no rebuild.
//! `chs --dump-lang` prints the full English set as a starting point.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

macro_rules! strings {
    ($($(#[doc = $doc:literal])* $key:ident = $default:expr;)*) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(default)]
        pub struct Strings {
            $($(#[doc = $doc])* pub $key: String,)*
        }
        impl Default for Strings {
            fn default() -> Self {
                Self { $($key: String::from($default),)* }
            }
        }
    };
}

strings! {
    // --- list / search ---
    search_placeholder = "type to search…";
    /// {shown} rows on screen, {total} sessions in the pool.
    counter = "{shown}/{total}";
    all_sources = "all";
    headless_on = "+headless";
    no_sessions = "No sessions found. Check the [[sources]] paths in {config}.";
    no_matches = "Nothing matches.";
    indexing = "Indexing sessions…";
    help = "↑↓ select · Enter resume · Tab source · Ctrl+T preview · Ctrl+A headless · Ctrl+O print command · Esc quit";
    // --- preview ---
    preview_title = "preview";
    prompts = "prompts";
    folder = "folder";
    session_id = "id";
    branch = "branch";
    started = "started";
    updated = "updated";
    file = "file";
    first_prompts = "first prompts";
    last_prompts = "latest prompts";
    best_match = "match";
    resume_with = "resume";
    headless_label = "non-interactive";
    more_prompts = "… {n} more …";
    // --- relative dates ---
    just_now = "now";
    minutes_ago = "{n}m";
    hours_ago = "{n}h";
    days_ago = "{n}d";
    // --- messages ---
    cwd_missing = "Folder {cwd} no longer exists, resuming from {fallback}.";
    cwd_missing_abort = "Folder {cwd} no longer exists.";
    launch_failed = "Could not start {cmd}: {err}";
    linked = "chs: linked {link} -> {target}";
    unlinked = "chs: removed {link}";
    link_fallback = "chs: {dir} is not writable, linked into {fallback} instead (make sure it is on PATH)";
    path_added = "chs: added {dir} to your PATH - new terminals will find `chs`";
    path_removed = "chs: removed {dir} from your PATH";
    config_created = "chs: created config {path}";
    // --- tray ---
    tray_open = "Open chs";
    tray_edit_config = "Edit config";
    tray_open_config_dir = "Open config folder";
    tray_autostart = "Start with Windows";
    tray_quit = "Quit";
    tray_disabled = "The tray is disabled in the config (windows.tray_enabled = false).";
}

impl Strings {
    pub fn load(file: &str) -> Result<Strings> {
        if file.trim().is_empty() {
            return Ok(Strings::default());
        }
        let path = crate::config::expand_path(file);
        Self::load_from(&path)
    }

    fn load_from(path: &Path) -> Result<Strings> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("can not read language file {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("language file {} is not valid", path.display()))
    }

    pub fn dump() -> String {
        let mut out = String::from(
            "# chs language file. Translate the values, keep the keys and the {placeholders}.\n\
             # Point general.language_file at this file. Missing keys fall back to English.\n\n",
        );
        out.push_str(&toml::to_string(&Strings::default()).expect("strings serialize"));
        out
    }
}

/// Replace `{name}` placeholders.
pub fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut s = template.to_string();
    for (k, v) in vars {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_translation_falls_back() {
        let s: Strings = toml::from_str("tray_quit = \"Вийти\"\nunknown_future_key = \"x\"\n").unwrap();
        assert_eq!(s.tray_quit, "Вийти");
        assert_eq!(s.tray_open, "Open chs");
    }

    #[test]
    fn dump_round_trips() {
        let s: Strings = toml::from_str(&Strings::dump()).unwrap();
        assert_eq!(s.help, Strings::default().help);
    }

    #[test]
    fn fill_works() {
        assert_eq!(fill("{shown}/{total}", &[("shown", "3"), ("total", "9")]), "3/9");
    }
}
