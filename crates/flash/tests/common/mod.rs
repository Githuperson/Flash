#![allow(dead_code)]
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub fn flash_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_flash"))
}

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

pub fn scratch_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("flash-test-{}-{}", std::process::id(), tag));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Build cache used by the tests, kept out of the user's real cache.
pub fn test_cache_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("flash-test-cache-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn flash(args: &[&str]) -> Output {
    Command::new(flash_bin())
        .args(args)
        .env("FLASH_PLUGIN_QUIET", "1")
        .env("FLASH_CACHE_DIR", test_cache_dir())
        .output()
        .expect("failed to run flash")
}

/// Build a program and run it; returns (build output, run output).
pub fn build_and_run(src: &Path, release: bool, tag: &str, run_args: &[&str]) -> (Output, Option<Output>) {
    build_and_run_with(src, release, None, tag, run_args)
}

pub fn clang_available() -> bool {
    Command::new(std::env::var("FLASH_CLANG").unwrap_or_else(|_| "clang".into()))
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn build_and_run_with(src: &Path, release: bool, backend: Option<&str>, tag: &str, run_args: &[&str]) -> (Output, Option<Output>) {
    let dir = scratch_dir(tag);
    let exe = dir.join(src.file_stem().unwrap());
    let mut args = vec!["build", src.to_str().unwrap(), "-o", exe.to_str().unwrap()];
    if release {
        args.push("--release");
    }
    if let Some(b) = backend {
        args.push("--backend");
        args.push(b);
    }
    let b = flash(&args);
    if !b.status.success() {
        return (b, None);
    }
    let r = Command::new(&exe)
        .args(run_args)
        .env("FLASH_RT_LEAKCHECK", "1")
        .env("FLASH_PLUGIN_QUIET", "1")
        .output()
        .expect("failed to run compiled program");
    let _ = std::fs::remove_dir_all(&dir);
    (b, Some(r))
}

pub fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}
