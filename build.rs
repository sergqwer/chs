//! Windows only: give both executables the app icon and version information.
//! The icon is rendered by the same code the tray uses (src/platform/icon.rs).

#[path = "src/platform/icon.rs"]
#[allow(dead_code)]
mod icon;

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/platform/icon.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let ico = out.join("chs.ico");
    std::fs::write(&ico, icon::ico(&[16, 20, 24, 32, 40, 48, 64, 256])).expect("write chs.ico");

    let v = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let mut parts = v.split(['.', '-']).map(|p| p.parse::<u16>().unwrap_or(0));
    let (a, b, c) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    // Forward slashes: both rc.exe and windres accept them, no escaping needed.
    let ico_path = ico.to_string_lossy().replace('\\', "/");
    let rc = format!(
        r#"1 ICON "{ico_path}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "sergqwer"
      VALUE "FileDescription", "chs - AI session history"
      VALUE "FileVersion", "{v}"
      VALUE "ProductName", "chs"
      VALUE "ProductVersion", "{v}"
      VALUE "LegalCopyright", "MIT License"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let rc_path = out.join("chs.rc");
    std::fs::write(&rc_path, rc).expect("write chs.rc");
    if let Err(e) = embed_resource::compile(&rc_path, embed_resource::NONE).manifest_optional() {
        println!("cargo:warning=could not embed the Windows icon: {e}");
    }
}
