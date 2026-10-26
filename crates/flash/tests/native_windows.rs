//! The Windows x86-64 executables (PE), run in a CPU emulator (scripts/run_win.py, which needs
//! python3 and the `unicorn` package and emulates the kernel32 functions the runtime imports; the
//! tests skip themselves when it is missing). Every program must print exactly what the Linux
//! build prints. On a real Windows machine the CI job runs the same programs natively.

mod common;
use common::*;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const TARGET: &str = "x86_64-pc-windows-msvc";

fn emulator_available() -> bool {
    Command::new("python3").args(["-c", "import unicorn"]).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

fn build_exe(src: &Path, release: bool, tag: &str) -> std::path::PathBuf {
    let exe = scratch_dir(tag).join("prog.exe");
    let mut args = vec!["build", src.to_str().unwrap(), "-o", exe.to_str().unwrap(), "--target", TARGET];
    if release {
        args.push("--release");
    }
    let b = flash(&args);
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    exe
}

/// Run in the emulator; returns (exit code, stdout).
fn emulate(exe: &Path, args: &[&str], stdin: &str) -> (i32, String) {
    let mut child = Command::new("python3")
        .arg(repo_root().join("scripts/run_win.py"))
        .arg(exe)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.code() != Some(139), "{}", String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
}

fn check_against_expected(src: &Path, tag: &str) {
    let expected = std::fs::read_to_string(src.with_extension("expected")).unwrap();
    for release in [false, true] {
        let exe = build_exe(src, release, &format!("win-{}-{}", tag, release));
        let (code, out) = emulate(&exe, &[], "");
        assert_eq!(code, 0, "{} (release: {})", tag, release);
        assert_eq!(out, expected, "{} (release: {})", tag, release);
    }
}

#[test]
fn examples_and_test_programs_run_on_windows() {
    if !emulator_available() {
        eprintln!("skipped: python3 with unicorn is not available");
        return;
    }
    for name in ["hello", "arithmetic", "conditions", "functions", "lists", "loops", "structs"] {
        check_against_expected(&repo_root().join("examples").join(format!("{}.flash", name)), name);
    }
    for name in ["integers", "strings", "ownership", "generics", "const_division", "methods", "tuples", "enums", "maps", "generic_types"] {
        check_against_expected(&repo_root().join("tests/programs").join(format!("{}.flash", name)), name);
    }
}

#[test]
fn arguments_environment_files_stdin_and_exit_codes_work_on_windows() {
    if !emulator_available() {
        eprintln!("skipped: python3 with unicorn is not available");
        return;
    }
    let src = repo_root().join("tests/native_io/io.flash");
    let want = std::fs::read_to_string(src.with_extension("expected")).unwrap();
    for release in [false, true] {
        let exe = build_exe(&src, release, &format!("win-io-{}", release));
        let (code, out) = emulate(&exe, &["a", "b c", "d"], "input line\n");
        assert_eq!(code, 3);
        assert_eq!(out, want, "release: {}", release);
    }
}

#[test]
fn benchmark_programs_match_the_linux_build_on_windows() {
    if !emulator_available() {
        eprintln!("skipped: python3 with unicorn is not available");
        return;
    }
    for name in ["strings", "sieve", "mandelbrot"] {
        let src = repo_root().join("benchmarks").join(format!("{}.flash", name));
        let (b, r) = build_and_run(&src, true, &format!("win-bench-linux-{}", name), &[]);
        assert!(b.status.success());
        let expected = String::from_utf8_lossy(&r.unwrap().stdout).into_owned();
        let exe = build_exe(&src, true, &format!("win-bench-{}", name));
        assert_eq!(emulate(&exe, &[], "").1, expected, "{}", name);
    }
}
