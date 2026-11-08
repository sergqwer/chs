//! Windows tray icon: left click opens chs in a terminal, the menu edits the config
//! and toggles autostart. It keeps nothing loaded: the config is read again on every
//! click, so edits apply without restarting the tray, and the session index is only
//! ever loaded by the chs.exe it starts.

use crate::config::{self, Terminal, TrayClick};
use crate::i18n::Strings;
use crate::platform::windows::{self as win, wide, CREATE_NEW_CONSOLE};
use anyhow::Result;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForSystem, GetSystemMetricsForDpi, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MessageBoxW, PostThreadMessageW, TranslateMessage, MB_ICONERROR, MB_OK, MSG,
    SM_CXSMICON, WM_APP,
};

enum Ev {
    Menu(MenuId),
    Click,
}

pub fn run() -> Result<()> {
    // One tray per user session. The handle stays open for the life of the process.
    let name = wide(win::TRAY_MUTEX);
    let _mutex = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Ok(());
    }

    let (cfg, _) = config::load()?;
    let t = Strings::load(&cfg.general.language_file).unwrap_or_default();
    win::sync(&cfg, &t);
    if !cfg.windows.tray_enabled {
        return Ok(());
    }
    // Without this Windows treats the tray as a 96-dpi app and stretches a 16 px icon
    // on scaled displays; DPI-aware, the icon is drawn at the exact size it is shown.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };

    let menu = Menu::new();
    let open = MenuItem::new(&t.tray_open, true, None);
    let edit = MenuItem::new(&t.tray_edit_config, true, None);
    let folder = MenuItem::new(&t.tray_open_config_dir, true, None);
    let auto = CheckMenuItem::new(&t.tray_autostart, true, cfg.windows.tray_autostart, None);
    let quit = MenuItem::new(&t.tray_quit, true, None);
    menu.append_items(&[
        &open,
        &PredefinedMenuItem::separator(),
        &edit,
        &folder,
        &auto,
        &PredefinedMenuItem::separator(),
        &quit,
    ])?;

    let left_opens = cfg.windows.tray_left_click == TrayClick::Open;
    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(&cfg.windows.tray_tooltip)
        .with_icon({
            let px = tray_icon_size();
            Icon::from_rgba(icon_rgba(px), px, px)?
        })
        .with_menu_on_left_click(!left_opens)
        .build()?;

    // The handlers run inside the window procedure; they only queue the event and wake
    // the message loop below, where the (non-Send) menu items can be used.
    let tid = unsafe { GetCurrentThreadId() };
    let (tx, rx) = mpsc::channel::<Ev>();
    let tx_click = tx.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = tx.send(Ev::Menu(e.id));
        unsafe { PostThreadMessageW(tid, WM_APP, 0, 0) };
    }));
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            if left_opens {
                let _ = tx_click.send(Ev::Click);
                unsafe { PostThreadMessageW(tid, WM_APP, 0, 0) };
            }
        }
    }));

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        if unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } <= 0 {
            return Ok(());
        }
        if msg.hwnd.is_null() && msg.message == WM_APP {
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    Ev::Click => open_chs(),
                    Ev::Menu(id) if id == *open.id() => open_chs(),
                    Ev::Menu(id) if id == *edit.id() => edit_config(),
                    Ev::Menu(id) if id == *folder.id() => open_config_dir(),
                    Ev::Menu(id) if id == *auto.id() => toggle_autostart(&auto),
                    Ev::Menu(id) if id == *quit.id() => return Ok(()),
                    Ev::Menu(_) => {}
                }
            }
            continue;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("C:\\"))
}

/// Open chs.exe - always by its full path, PATH is never involved.
fn open_chs() {
    let cfg = match config::load() {
        Ok((c, _)) => c,
        Err(e) => return error_box(&format!("{e:#}")),
    };
    let Some(chs) = win::chs_exe() else { return };
    if !chs.exists() {
        return error_box(&format!("{} not found next to chs-tray.exe", chs.display()));
    }
    let w = &cfg.windows;
    let home = home();
    let default_terminal = || Command::new(&chs).current_dir(&home).creation_flags(CREATE_NEW_CONSOLE).spawn();
    let res = match w.terminal {
        Terminal::Auto => default_terminal(),
        Terminal::Conhost => Command::new("conhost.exe").arg(&chs).current_dir(&home).spawn(),
        Terminal::Wt => Command::new("wt.exe")
            .args(["-w", "new", "--title", "chs", "-d"])
            .arg(&home)
            .arg(&chs)
            .spawn()
            .or_else(|_| default_terminal()),
        Terminal::Custom => {
            let chs_s = chs.to_string_lossy();
            let argv: Vec<String> = w.terminal_command.iter().map(|a| a.replace("{chs}", &chs_s)).collect();
            match argv.split_first() {
                Some((prog, args)) => Command::new(prog).args(args).current_dir(&home).spawn(),
                None => default_terminal(),
            }
        }
    };
    if let Err(e) = res {
        error_box(&format!("could not open a terminal: {e}"));
    }
}

fn edit_config() {
    let editor = config::load().map(|(c, _)| c.windows.editor).unwrap_or_else(|_| "notepad.exe".into());
    let path = config::config_path();
    if Command::new(&editor).arg(&path).spawn().is_err() {
        let _ = Command::new("notepad.exe").arg(&path).spawn();
    }
}

fn open_config_dir() {
    if let Some(dir) = config::config_path().parent() {
        let _ = Command::new("explorer.exe").arg(dir).spawn();
    }
}

/// The menu checkbox writes `windows.tray_autostart` into the config (comments kept),
/// then the registry is synced from the config like on any start.
fn toggle_autostart(item: &CheckMenuItem) {
    let on = item.is_checked();
    let path = config::config_path();
    let result = std::fs::read_to_string(&path)
        .map_err(anyhow::Error::from)
        .and_then(|text| {
            let new = config::set_bool(&text, "windows", "tray_autostart", on);
            config::Config::from_user_str(&new)?;
            std::fs::write(&path, new)?;
            Ok(())
        })
        .and_then(|_| config::load());
    match result {
        Ok((cfg, _)) => {
            let t = Strings::load(&cfg.general.language_file).unwrap_or_default();
            win::sync(&cfg, &t);
            item.set_checked(cfg.windows.tray_autostart);
        }
        Err(e) => {
            item.set_checked(!on);
            error_box(&format!("{e:#}"));
        }
    }
}

pub fn error_box(text: &str) {
    let text = wide(text);
    let title = wide("chs");
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
}

pub fn icon_rgba(size: u32) -> Vec<u8> {
    crate::platform::icon::rgba(size)
}

/// Pixel size of a small icon at the system DPI: 16 at 100 %, 20 at 125 %, 24 at 150 %.
/// The icon is set once at start; nothing redraws it afterwards.
fn tray_icon_size() -> u32 {
    let px = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) };
    if px <= 0 {
        32
    } else {
        (px as u32).clamp(16, 64)
    }
}
