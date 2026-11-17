use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let assets = manifest.join("assets");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let res = out.join("icon.res");
    let obj = out.join("icon.obj");

    let machine = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x64",
        Ok("aarch64") => "arm64",
        Ok("x86") => "x86",
        _ => "x64",
    };

    let status = Command::new("llvm-rc")
        .current_dir(&assets)
        .args(["/fo", res.to_str().expect("utf-8 OUT_DIR"), "icon.rc"])
        .status()
        .expect("llvm-rc is required to build the Windows icon resource");
    assert!(status.success(), "llvm-rc failed");

    let status = Command::new("llvm-cvtres")
        .args([
            &format!("/machine:{machine}"),
            &format!("/out:{}", obj.display()),
            res.to_str().expect("utf-8 OUT_DIR"),
        ])
        .status()
        .expect("llvm-cvtres is required to build the Windows icon resource");
    assert!(status.success(), "llvm-cvtres failed");

    println!("cargo:rustc-link-arg={}", obj.display());
}
