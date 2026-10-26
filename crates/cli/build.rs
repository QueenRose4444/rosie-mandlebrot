//! Builds the icon into the Windows exe (Explorer, the taskbar, Alt+Tab):
//! `assets/icon.rc` compiled by MinGW's `windres` (set `WINDRES` to use
//! another) and linked into the binary. Other targets have no icon to embed.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-env-changed=WINDRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu") {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("icon.o");
    let windres = std::env::var("WINDRES").unwrap_or_else(|_| "windres".into());
    let ok = Command::new(&windres)
        .args(["--input", "assets/icon.rc", "--output-format=coff", "--output"])
        .arg(&out)
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        println!("cargo:rustc-link-arg-bins={}", out.display());
    } else {
        println!("cargo:warning=`{windres}` did not run: the exe is built without its icon");
    }
}
