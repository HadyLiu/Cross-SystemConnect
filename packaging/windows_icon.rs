//! Embed `resources/app.ico` into a Windows binary. Used from each crate's `build.rs`.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub fn embed(bin: &str) {
    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        return;
    }
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let icon = manifest.join("../../resources/app.ico");
    let icon = fs::canonicalize(&icon).unwrap_or(icon);
    println!("cargo:rerun-if-changed={}", icon.display());

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let rc_path = out_dir.join("app.rc");
    let icon_arg = icon.display().to_string().replace('\\', "/");
    fs::write(&rc_path, format!("1 ICON \"{icon_arg}\"\n")).expect("write icon.rc");

    let obj = out_dir.join("app.res");
    let windres = env::var("WINDRES").unwrap_or_else(|_| {
        let target = env::var("TARGET").unwrap_or_default();
        if target.contains("windows-gnu") {
            "x86_64-w64-mingw32-windres".into()
        } else {
            "windres".into()
        }
    });
    let status = Command::new(&windres)
        .arg(&rc_path)
        .args(["-O", "coff", "-o"])
        .arg(&obj)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {windres}: {e}"));
    if !status.success() {
        panic!("{windres} failed to embed the application icon");
    }
    println!("cargo:rustc-link-arg-bin={bin}={}", obj.display());
}
