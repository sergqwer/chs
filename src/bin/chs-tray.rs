//! The Windows tray icon. A GUI-subsystem binary, so starting it (at logon, from
//! chs.exe) never flashes a console window.

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    if let Err(e) = chs::tray::run() {
        chs::tray::error_box(&format!("{e:#}"));
        std::process::exit(1);
    }
    #[cfg(not(windows))]
    {
        eprintln!("chs-tray is the Windows tray icon; on this platform just run `chs`.");
        std::process::exit(1);
    }
}
