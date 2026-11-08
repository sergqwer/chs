//! Linux / macOS: make `chs` callable from any shell by linking the running binary
//! into a directory that is on the default PATH (`/usr/local/bin`), with `sudo -n`
//! when needed. Turning `linux.add_to_path` off removes the link again, but only a
//! link that points at this binary is ever touched.

use crate::config::{expand_path, Config};
use crate::i18n::{fill, Strings};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn sync(cfg: &Config, t: &Strings) -> Vec<String> {
    let mut notes = Vec::new();
    let Some(exe) = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok()) else {
        return notes;
    };
    // `cargo run` / `cargo test` builds are not something to install.
    if exe.components().any(|c| c.as_os_str() == "debug" || c.as_os_str() == "deps") {
        return notes;
    }
    let lc = &cfg.linux;
    let primary = expand_path(&lc.link_dir).join("chs");
    let fallback = expand_path(&lc.fallback_dir).join("chs");

    if lc.add_to_path {
        if points_to(&primary, &exe) || points_to(&fallback, &exe) {
            return notes;
        }
        // Never replace a real file: someone installed chs there on purpose.
        if primary.exists() && !is_symlink(&primary) {
            return notes;
        }
        if link(&exe, &primary, lc.use_sudo) {
            notes.push(fill(&t.linked, &[("link", &primary.to_string_lossy()), ("target", &exe.to_string_lossy())]));
        } else if (!fallback.exists() || is_symlink(&fallback)) && link(&exe, &fallback, false) {
            notes.push(fill(
                &t.link_fallback,
                &[("dir", &lc.link_dir), ("fallback", &fallback.parent().unwrap_or(Path::new("")).to_string_lossy())],
            ));
        }
    } else {
        for l in [&primary, &fallback] {
            if points_to(l, &exe) && unlink(l, lc.use_sudo) {
                notes.push(fill(&t.unlinked, &[("link", &l.to_string_lossy())]));
            }
        }
    }
    notes
}

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

fn points_to(link: &Path, exe: &Path) -> bool {
    is_symlink(link) && link.canonicalize().is_ok_and(|t| t == exe)
}

fn link(exe: &Path, at: &Path, sudo: bool) -> bool {
    let Some(dir) = at.parent() else { return false };
    if std::fs::create_dir_all(dir).is_ok() {
        // Create next to the target and rename over it: atomic, and replaces a stale link.
        let tmp: PathBuf = dir.join(format!(".chs.{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        if std::os::unix::fs::symlink(exe, &tmp).is_ok() {
            if std::fs::rename(&tmp, at).is_ok() {
                return true;
            }
            let _ = std::fs::remove_file(&tmp);
        }
    }
    sudo && run_sudo(&["ln", "-sfn", &exe.to_string_lossy(), &at.to_string_lossy()])
}

fn unlink(at: &Path, sudo: bool) -> bool {
    std::fs::remove_file(at).is_ok() || (sudo && run_sudo(&["rm", "-f", &at.to_string_lossy()]))
}

fn run_sudo(args: &[&str]) -> bool {
    Command::new("sudo")
        .arg("-n")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
