//! Windows: keep the user PATH and the tray autostart entry in line with the config,
//! and start the tray. Everything is per-user (HKCU), so no administrator rights.
//!
//! Both chs.exe and chs-tray.exe call [`sync`] on start, so the first run of either
//! one registers everything, and switching an option off removes what it added.

use crate::config::{cache_root, Config};
use crate::i18n::{fill, Strings};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS,
};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_EXPAND_SZ, REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
};

pub const TRAY_MUTEX: &str = "Local\\chs-tray-single-instance";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "chs";
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
pub const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

pub fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// The folder holding chs.exe and chs-tray.exe (whichever of the two is running).
pub fn install_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

pub fn chs_exe() -> Option<PathBuf> {
    install_dir().map(|d| d.join("chs.exe"))
}

pub fn tray_exe() -> Option<PathBuf> {
    install_dir().map(|d| d.join("chs-tray.exe"))
}

#[derive(Serialize, Deserialize, Default)]
struct State {
    /// The PATH entry chs itself inserted. Only this one is ever removed again: a folder
    /// that was already on PATH (say C:\Tools holding chs.exe) belongs to the user.
    added_path: Option<String>,
}

fn state_path() -> PathBuf {
    cache_root().join("state.json")
}

fn read_state() -> State {
    std::fs::read(state_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_state(s: &State) {
    let p = state_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, serde_json::to_vec(s).unwrap_or_default());
}

pub fn sync(cfg: &Config, t: &Strings) -> Vec<String> {
    let mut notes = Vec::new();
    let Some(dir) = install_dir() else { return notes };
    if dir.components().any(|c| c.as_os_str() == "debug" || c.as_os_str() == "deps") {
        return notes;
    }
    let dir_s = dir.to_string_lossy().into_owned();
    let w = &cfg.windows;

    // --- PATH ---
    // read_user_path() is None whenever the value could not be read reliably; PATH is
    // then left alone, since writing back a list we failed to read would wipe it.
    if let Some((mut entries, vtype)) = read_user_path() {
        let mut state = read_state();
        let mut changed = false;
        let mut owned = state.added_path.clone();
        // A moved install: drop the entry chs added for the old folder.
        if let Some(prev) = owned.clone() {
            if !same_dir(&prev, &dir_s) {
                changed |= remove_entry(&mut entries, &prev);
                owned = None;
            }
        }
        if w.add_to_path {
            if !entries.iter().any(|e| same_dir(e, &dir_s)) {
                entries.push(dir_s.clone());
                changed = true;
                owned = Some(dir_s.clone());
                notes.push(fill(&t.path_added, &[("dir", &dir_s)]));
            }
        } else if owned.as_deref().is_some_and(|o| same_dir(o, &dir_s)) {
            if remove_entry(&mut entries, &dir_s) {
                changed = true;
                notes.push(fill(&t.path_removed, &[("dir", &dir_s)]));
            }
            owned = None;
        }
        let ok = !changed || write_user_path(&entries, vtype);
        if changed && ok {
            broadcast_env_change();
        }
        if !ok {
            notes.clear();
        } else if owned != state.added_path {
            state.added_path = owned;
            write_state(&state);
        }
    }

    // --- autostart ---
    let tray = dir.join("chs-tray.exe");
    let want = w.tray_enabled && w.tray_autostart && tray.exists();
    let value = format!("\"{}\"", tray.display());
    let current = reg_get(RUN_KEY, RUN_VALUE).map(|(v, _)| v);
    if want {
        if current.as_deref() != Some(value.as_str()) {
            reg_set(RUN_KEY, RUN_VALUE, &value, REG_SZ);
        }
    } else if current.is_some() {
        reg_delete(RUN_KEY, RUN_VALUE);
    }
    notes
}

/// Start chs-tray.exe (full path, detached) unless it is disabled or already running.
pub fn start_tray_if_wanted(cfg: &Config) {
    let w = &cfg.windows;
    if !w.tray_enabled || !w.start_tray_with_cli || tray_running() {
        return;
    }
    if let Some(tray) = tray_exe().filter(|p| p.exists()) {
        spawn_detached(&tray);
    }
}

/// CreateProcess with `bInheritHandles = FALSE`. std's Command always passes TRUE, so
/// the long-lived tray would inherit our stdout/stderr - and whoever reads them (a
/// pipe, an SSH session, a script capturing the output) would wait until the tray exits.
fn spawn_detached(exe: &Path) -> bool {
    let app = wide(&exe.to_string_lossy());
    let mut cmdline = wide(&format!("\"{}\"", exe.display()));
    let cwd = exe.parent().map(|p| wide(&p.to_string_lossy()));
    unsafe {
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessW(
            app.as_ptr(),
            cmdline.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
            std::ptr::null(),
            cwd.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            &si,
            &mut pi,
        );
        if ok != 0 {
            CloseHandle(pi.hProcess);
            CloseHandle(pi.hThread);
        }
        ok != 0
    }
}

pub fn tray_running() -> bool {
    let name = wide(TRAY_MUTEX);
    unsafe {
        let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        let exists = GetLastError() == ERROR_ALREADY_EXISTS;
        if !h.is_null() {
            CloseHandle(h);
        }
        exists
    }
}

fn norm_dir(s: &str) -> String {
    crate::config::expand_path(s).to_string_lossy().trim_end_matches(['\\', '/']).replace('/', "\\").to_lowercase()
}

fn same_dir(a: &str, b: &str) -> bool {
    norm_dir(a) == norm_dir(b)
}

fn remove_entry(entries: &mut Vec<String>, dir: &str) -> bool {
    let before = entries.len();
    entries.retain(|e| !same_dir(e, dir));
    entries.len() != before
}

/// `None` = the value exists but could not be read: PATH is then left alone, because
/// writing back a list we failed to read would wipe the user's PATH.
fn read_user_path() -> Option<(Vec<String>, REG_VALUE_TYPE)> {
    match reg_query("Environment", "Path") {
        Ok(Some((v, ty))) => Some((v.split(';').filter(|e| !e.trim().is_empty()).map(str::to_string).collect(), ty)),
        // No user PATH yet: the key exists on every account, the value may not.
        Ok(None) => Some((Vec::new(), REG_EXPAND_SZ)),
        Err(()) => None,
    }
}

fn write_user_path(entries: &[String], vtype: REG_VALUE_TYPE) -> bool {
    let ty = if vtype == REG_SZ { REG_SZ } else { REG_EXPAND_SZ };
    reg_set("Environment", "Path", &entries.join(";"), ty)
}

/// Tell Explorer (and so every terminal started from now on) that PATH changed.
fn broadcast_env_change() {
    let env = wide("Environment");
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            env.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            3000,
            std::ptr::null_mut(),
        );
    }
}

/// Err carries the Win32 error code, so "no such key" can be told from "access denied".
fn open_key(sub: &str, access: u32) -> Result<HKEY, u32> {
    let sub = wide(sub);
    let mut key: HKEY = std::ptr::null_mut();
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, access, &mut key) };
    if rc == ERROR_SUCCESS {
        Ok(key)
    } else {
        Err(rc)
    }
}

/// Open for writing, creating the key if this account does not have it yet.
fn create_key(sub: &str) -> Option<HKEY> {
    let sub = wide(sub);
    let mut key: HKEY = std::ptr::null_mut();
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    (rc == ERROR_SUCCESS).then_some(key)
}

fn reg_get(sub: &str, name: &str) -> Option<(String, REG_VALUE_TYPE)> {
    reg_query(sub, name).ok().flatten()
}

/// `Ok(None)` = no such value, `Err` = it exists but is unreadable or not a string.
fn reg_query(sub: &str, name: &str) -> Result<Option<(String, REG_VALUE_TYPE)>, ()> {
    let key = match open_key(sub, KEY_QUERY_VALUE) {
        Ok(k) => k,
        Err(ERROR_FILE_NOT_FOUND) => return Ok(None),
        Err(_) => return Err(()),
    };
    let name = wide(name);
    let mut ty: REG_VALUE_TYPE = 0;
    let mut len: u32 = 0;
    let out = unsafe {
        let rc = RegQueryValueExW(key, name.as_ptr(), std::ptr::null(), &mut ty, std::ptr::null_mut(), &mut len);
        if rc == ERROR_FILE_NOT_FOUND {
            Ok(None)
        } else if rc != ERROR_SUCCESS || (ty != REG_SZ && ty != REG_EXPAND_SZ) {
            Err(())
        } else {
            let mut buf = vec![0u16; (len as usize).div_ceil(2) + 1];
            let mut blen = (buf.len() * 2) as u32;
            let rc =
                RegQueryValueExW(key, name.as_ptr(), std::ptr::null(), &mut ty, buf.as_mut_ptr().cast(), &mut blen);
            if rc != ERROR_SUCCESS {
                Err(())
            } else {
                let n = (blen as usize / 2).min(buf.len());
                let s = String::from_utf16_lossy(&buf[..n]);
                Ok(Some((s.trim_end_matches('\0').to_string(), ty)))
            }
        }
    };
    unsafe { RegCloseKey(key) };
    out
}

fn reg_set(sub: &str, name: &str, value: &str, ty: REG_VALUE_TYPE) -> bool {
    let Some(key) = create_key(sub) else { return false };
    let name = wide(name);
    let data = wide(value);
    let rc = unsafe { RegSetValueExW(key, name.as_ptr(), 0, ty, data.as_ptr().cast(), (data.len() * 2) as u32) };
    unsafe { RegCloseKey(key) };
    rc == ERROR_SUCCESS
}

fn reg_delete(sub: &str, name: &str) -> bool {
    let Ok(key) = open_key(sub, KEY_SET_VALUE) else { return false };
    let name = wide(name);
    let rc = unsafe { RegDeleteValueW(key, name.as_ptr()) };
    unsafe { RegCloseKey(key) };
    rc == ERROR_SUCCESS
}

/// `chs --uninstall`: remove the PATH entry chs added and the autostart value.
pub fn uninstall() -> Vec<String> {
    let mut notes = Vec::new();
    let state = read_state();
    if let (Some(added), Some((mut entries, ty))) = (state.added_path.as_deref(), read_user_path()) {
        if remove_entry(&mut entries, added) && write_user_path(&entries, ty) {
            broadcast_env_change();
            notes.push(format!("removed {added} from PATH"));
        }
    }
    if reg_delete(RUN_KEY, RUN_VALUE) {
        notes.push("removed the autostart entry".into());
    }
    write_state(&State::default());
    notes
}
