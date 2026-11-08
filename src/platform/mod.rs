//! Making `chs` reachable: global PATH on every OS, plus the tray autostart on Windows.

pub mod icon;
#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
pub mod windows;

use crate::config::Config;
use crate::i18n::Strings;

/// Bring the system in line with the config. Returns one-line notices of what changed.
pub fn sync(cfg: &Config, t: &Strings) -> Vec<String> {
    #[cfg(unix)]
    return unix::sync(cfg, t);
    #[cfg(windows)]
    return windows::sync(cfg, t);
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (cfg, t);
        Vec::new()
    }
}
