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
    build_and_run_extra(src, release, backend, &[], tag, run_args)
}

/// Like `build_and_run_with` with extra `flash build` arguments (for example `--linker system`).
pub fn build_and_run_extra(
    src: &Path,
    release: bool,
    backend: Option<&str>,
    extra: &[&str],
    tag: &str,
    run_args: &[&str],
) -> (Output, Option<Output>) {
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
    args.extend_from_slice(extra);
    let b = flash(&args);
    if !b.status.success() {
        return (b, None);
    }
    let r = run_with_timeout(Command::new(&exe).args(run_args).env("FLASH_RT_LEAKCHECK", "1").env("FLASH_PLUGIN_QUIET", "1"), 60);
    let _ = std::fs::remove_dir_all(&dir);
    (b, Some(r))
}

pub fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Run a program and collect its output; a program still running after `secs` seconds is killed and
/// reported with exit code 124 (so a hang fails one test instead of blocking the whole run).
pub fn run_with_timeout(cmd: &mut Command, secs: u64) -> Output {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("failed to run compiled program");
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let t_out = std::thread::spawn(move || {
        let mut v = vec![];
        let _ = out.read_to_end(&mut v);
        v
    });
    let t_err = std::thread::spawn(move || {
        let mut v = vec![];
        let _ = err.read_to_end(&mut v);
        v
    });
    let start = std::time::Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait().expect("wait failed") {
            Some(s) => break s,
            None if start.elapsed().as_secs() >= secs => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().expect("wait failed");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    };
    let stdout = t_out.join().unwrap_or_default();
    let mut stderr = t_err.join().unwrap_or_default();
    if timed_out {
        stderr.extend_from_slice(format!("\n[test harness] killed after {} seconds (hang?)\n", secs).as_bytes());
    }
    #[cfg(unix)]
    let status = if timed_out {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(124 << 8)
    } else {
        status
    };
    Output { status, stdout, stderr }
}
