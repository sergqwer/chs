//! Resuming a session: build the command from the source's template and run it in
//! the session's own folder (Claude Code only finds a session from the folder it was
//! started in).

use crate::config::{AfterResume, Config, MissingCwd};
use crate::i18n::{fill, Strings};
use crate::session::Session;
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Launch {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    /// Shown before starting, e.g. that the folder is gone.
    pub note: Option<String>,
    /// `cwd` is gone and is made again by `make_cwd`, just before the start: showing
    /// the command (preview, `--list --json`) must not touch the disk.
    pub create: bool,
}

pub fn prepare(cfg: &Config, t: &Strings, s: &Session) -> Result<Launch> {
    let src = cfg.source(&s.source).ok_or_else(|| anyhow!("unknown source {}", s.source))?;
    let argv: Vec<String> = src
        .resume_command
        .iter()
        .map(|a| a.replace("{id}", &s.id).replace("{cwd}", &s.cwd).replace("{file}", &s.file.to_string_lossy()))
        .collect();
    let path = Path::new(&s.cwd);
    if !s.cwd.is_empty() && path.is_dir() {
        return Ok(Launch { argv, cwd: path.to_path_buf(), note: None, create: false });
    }
    let mode = cfg.general.missing_cwd;
    // Only a path that is absolute on this OS: "/home/u/x" on Windows would land in C:\.
    if mode == MissingCwd::Recreate && path.is_absolute() {
        let note = Some(fill(&t.cwd_recreated, &[("cwd", &s.cwd)]));
        return Ok(Launch { argv, cwd: path.to_path_buf(), note, create: true });
    }
    let fallback = match mode {
        MissingCwd::Recreate | MissingCwd::Home => home(),
        MissingCwd::Current => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        MissingCwd::Abort => return Err(anyhow!(fill(&t.cwd_missing_abort, &[("cwd", &s.cwd)]))),
    };
    let note = Some(fill(&t.cwd_missing, &[("cwd", &s.cwd), ("fallback", &fallback.to_string_lossy())]));
    Ok(Launch { argv, cwd: fallback, note, create: false })
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Make the gone folder again (with its parents) when `prepare` asked for it; if that
/// fails, resume from the home folder instead.
pub fn make_cwd(l: &mut Launch, t: &Strings) {
    if !std::mem::take(&mut l.create) {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&l.cwd) {
        let fallback = home();
        l.note = Some(fill(
            &t.cwd_recreate_failed,
            &[("cwd", &l.cwd.to_string_lossy()), ("err", &e.to_string()), ("fallback", &fallback.to_string_lossy())],
        ));
        l.cwd = fallback;
    }
}

/// A copy-pasteable command line.
pub fn display(l: &Launch) -> String {
    let args = l.argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ");
    let dir = quote(&l.cwd.to_string_lossy());
    // mkdir makes the parents too in cmd and PowerShell; pushd changes drive as well.
    let (mkdir, cd) = if cfg!(windows) { ("mkdir", "pushd") } else { ("mkdir -p", "cd") };
    if l.create {
        format!("{mkdir} {dir} && {cd} {dir} && {args}")
    } else {
        format!("{cd} {dir} && {args}")
    }
}

fn quote(s: &str) -> String {
    // Backslash is a path separator on Windows but an escape in sh; a leading ~ would
    // expand in sh. Anything outside these sets gets quoted.
    let safe = if cfg!(windows) { "-_./:=@+,\\" } else { "-_./:=@%+," };
    let plain = !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || safe.contains(c));
    if plain {
        s.to_string()
    } else if cfg!(windows) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Run it. With `AfterResume::Exit` on Unix this never returns on success: chs is
/// replaced by the resumed session and holds no memory while you work.
pub fn run(l: &Launch, after: AfterResume) -> Result<i32> {
    let (prog, args) = resolve(&l.argv)?;
    let mut cmd = Command::new(&prog);
    cmd.args(&args).current_dir(&l.cwd);

    #[cfg(unix)]
    if after == AfterResume::Exit {
        use std::os::unix::process::CommandExt;
        let err = cmd.exec();
        return Err(anyhow!("{}: {err}", prog.to_string_lossy()));
    }
    let _ = after;

    #[cfg(windows)]
    ignore_ctrl_c();
    #[cfg(unix)]
    let _guard = SigintGuard::install();

    let status = cmd.status().map_err(|e| anyhow!("{}: {e}", prog.to_string_lossy()))?;
    Ok(status.code().unwrap_or(1))
}

/// While a resumed session runs and chs waits to show the list again, Ctrl+C is meant
/// for the session. chs catches SIGINT with a do-nothing handler instead of ignoring
/// it: an ignored signal stays ignored across exec, a handler is reset to the default,
/// so the child still gets normal Ctrl+C. The old action comes back on drop.
#[cfg(unix)]
struct SigintGuard(libc::sigaction);

#[cfg(unix)]
impl SigintGuard {
    fn install() -> Option<SigintGuard> {
        extern "C" fn noop(_: libc::c_int) {}
        unsafe {
            let mut new: libc::sigaction = std::mem::zeroed();
            new.sa_sigaction = noop as *const () as libc::sighandler_t;
            libc::sigemptyset(&mut new.sa_mask);
            let mut old: libc::sigaction = std::mem::zeroed();
            (libc::sigaction(libc::SIGINT, &new, &mut old) == 0).then_some(SigintGuard(old))
        }
    }
}

#[cfg(unix)]
impl Drop for SigintGuard {
    fn drop(&mut self) {
        unsafe { libc::sigaction(libc::SIGINT, &self.0, std::ptr::null_mut()) };
    }
}

#[cfg(unix)]
fn resolve(argv: &[String]) -> Result<(PathBuf, Vec<String>)> {
    let (first, rest) = argv.split_first().ok_or_else(|| anyhow!("empty resume_command"))?;
    Ok((PathBuf::from(first), rest.to_vec()))
}

/// `Command::new("claude")` on Windows only looks for `claude.exe`; npm installs a
/// `claude.cmd`. Resolve through PATH/PATHEXT first. Rust's std runs .cmd/.bat files
/// itself (with safe argument quoting); PowerShell scripts need powershell.exe.
#[cfg(windows)]
fn resolve(argv: &[String]) -> Result<(PathBuf, Vec<String>)> {
    let (first, rest) = argv.split_first().ok_or_else(|| anyhow!("empty resume_command"))?;
    let found =
        which::which(first).or_else(|_| which::which(format!("{first}.ps1"))).unwrap_or_else(|_| PathBuf::from(first));
    let ext = found.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == "ps1" {
        let mut args = vec![
            "-NoLogo".to_string(),
            "-NoProfile".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-File".into(),
            found.to_string_lossy().into_owned(),
        ];
        args.extend(rest.iter().cloned());
        return Ok((PathBuf::from("powershell.exe"), args));
    }
    Ok((found, rest.to_vec()))
}

/// While the child runs, Ctrl+C belongs to it. A handler that swallows the event
/// keeps chs alive (so the window stays) without the "ignore Ctrl+C" flag, which the
/// child would inherit.
#[cfg(windows)]
fn ignore_ctrl_c() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    unsafe extern "system" fn swallow(_ctrl: u32) -> i32 {
        1
    }
    unsafe {
        SetConsoleCtrlHandler(Some(swallow), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_and_quoting() {
        let cfg = Config::default_config();
        let t = Strings::default();
        let s = Session {
            source: "codex".into(),
            id: "01a1-x".into(),
            cwd: "/definitely/not/here".into(),
            ..Default::default()
        };
        let l = prepare(&cfg, &t, &s).unwrap();
        assert_eq!(l.argv, vec!["codex", "resume", "01a1-x"]);
        assert!(l.note.is_some(), "a missing folder is announced");
        assert!(display(&l).ends_with("codex resume 01a1-x"));
        assert_eq!(quote("a b'c"), if cfg!(windows) { "\"a b'c\"" } else { "'a b'\\''c'" });
        if cfg!(unix) {
            assert_eq!(quote("/tmp/a\\b"), "'/tmp/a\\b'");
            assert_eq!(quote("~/x"), "'~/x'");
        }
    }

    #[test]
    fn gone_folder_is_made_again_only_on_resume() {
        let cfg = Config::default_config();
        let t = Strings::default();
        let root = std::env::temp_dir().join(format!("chs-launch-{}", std::process::id()));
        let gone = root.join("deleted/project");
        let s = Session {
            source: "claude".into(),
            id: "s1".into(),
            cwd: gone.to_string_lossy().into(),
            ..Default::default()
        };
        let mut l = prepare(&cfg, &t, &s).unwrap();
        assert!(l.create && l.cwd == gone, "same path, made later");
        assert!(display(&l).starts_with(if cfg!(windows) { "mkdir " } else { "mkdir -p " }));
        assert!(!gone.exists(), "preparing (preview, --json) does not touch the disk");
        make_cwd(&mut l, &t);
        assert!(gone.is_dir() && !l.create);
        let again = prepare(&cfg, &t, &s).unwrap();
        assert!(!again.create && again.note.is_none());
        std::fs::remove_dir_all(&root).unwrap();

        // Not absolute here (a path from the other OS): never made, home instead.
        let foreign = if cfg!(windows) { "/home/u/proj" } else { "C:\\Users\\u\\proj" };
        let s = Session { cwd: foreign.into(), ..s };
        let l = prepare(&cfg, &t, &s).unwrap();
        assert!(!l.create && l.cwd == home());
    }
}
