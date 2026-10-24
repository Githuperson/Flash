//! flashpkg: local packages, lock files, integrity checks, native package requirements.

mod common;
use common::*;
use std::path::Path;
use std::process::{Command, Output};

fn pkg_bin() -> std::path::PathBuf {
    // flashpkg is built next to flash
    flash_bin().with_file_name(if cfg!(windows) { "flashpkg.exe" } else { "flashpkg" })
}

fn flashpkg(dir: &Path, args: &[&str]) -> Output {
    // make sure the sibling binary exists when only `cargo test -p flash` was run
    Command::new(pkg_bin())
        .current_dir(dir)
        .args(args)
        .env("FLASH_PLUGIN_QUIET", "1")
        .output()
        .expect("flashpkg binary not built; run `cargo build --workspace`")
}

fn write(p: impl AsRef<Path>, s: &str) {
    let p = p.as_ref();
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

#[test]
fn init_add_install_build_run_and_tamper_detection() {
    let base = scratch_dir("pkg-flow");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    // a library package
    write(base.join("mylib/flash.toml"), "[package]\nname = \"mylib\"\nversion = \"1.2.0\"\nkind = \"library\"\n");
    write(base.join("mylib/src/mylib.flash"), "fn triple(x):\n    return x * 3\n");
    // an app
    assert!(flashpkg(&base, &["init", "app"]).status.success());
    let app = base.join("app");
    assert!(app.join("flash.toml").is_file() && app.join("src/main.flash").is_file());
    let add = flashpkg(&app, &["add", "mylib", "--path", "../mylib"]);
    assert!(add.status.success(), "{}", text(&add.stderr));
    let lock = std::fs::read_to_string(app.join("flash.lock")).unwrap();
    assert!(lock.contains("name = \"mylib\"") && lock.contains("integrity = \"sha256:"), "{}", lock);
    write(app.join("src/main.flash"), "import mylib\nsay(mylib.triple(14))\n");
    // building twice gives an identical lock file (reproducible)
    assert!(flashpkg(&app, &["install"]).status.success());
    assert_eq!(std::fs::read_to_string(app.join("flash.lock")).unwrap(), lock);
    let run = flashpkg(&app, &["run"]);
    assert!(run.status.success(), "{}", text(&run.stderr));
    assert!(text(&run.stdout).contains("42"), "{}", text(&run.stdout));
    // editing the app does not invalidate the lock; editing a dependency does
    write(app.join("src/main.flash"), "import mylib\nsay(mylib.triple(5))\n");
    assert!(text(&flashpkg(&app, &["run"]).stdout).contains("15"));
    write(base.join("mylib/src/mylib.flash"), "fn triple(x):\n    return x * 30\n");
    let tampered = flashpkg(&app, &["build"]);
    assert_eq!(tampered.status.code(), Some(1));
    assert!(text(&tampered.stderr).contains("'mylib' has changed since it was locked"), "{}", text(&tampered.stderr));
    assert_eq!(flashpkg(&app, &["verify"]).status.code(), Some(1));
    // re-locking accepts the new content
    assert!(flashpkg(&app, &["install"]).status.success());
    assert!(text(&flashpkg(&app, &["run"]).stdout).contains("150"));
    // tests directory
    let t = flashpkg(&app, &["test"]);
    assert!(t.status.success(), "{}{}", text(&t.stdout), text(&t.stderr));
}

#[test]
fn only_local_path_dependencies_are_supported_and_names_must_match() {
    let base = scratch_dir("pkg-names");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    assert!(flashpkg(&base, &["init", "app"]).status.success());
    let app = base.join("app");
    let r = flashpkg(&app, &["add", "somepkg"]);
    assert_eq!(r.status.code(), Some(1));
    assert!(text(&r.stderr).contains("no registry"), "{}", text(&r.stderr));
    write(base.join("lib/flash.toml"), "[package]\nname = \"other\"\nversion = \"0.1.0\"\n");
    let r = flashpkg(&app, &["add", "lib", "--path", "../lib"]);
    assert_eq!(r.status.code(), Some(1));
    assert!(text(&r.stderr).contains("contains the package 'other'"), "{}", text(&r.stderr));
    assert_eq!(flashpkg(&base, &["init", "Bad Name"]).status.code(), Some(0)); // normalised to bad_name
    assert!(base.join("Bad Name/flash.toml").is_file());
}

#[test]
fn native_package_requirements_are_enforced() {
    let base = scratch_dir("pkg-native");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    assert!(flashpkg(&base, &["init", "app"]).status.success());
    let app = base.join("app");
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    // wrong OS
    write(
        base.join("nat/flash.toml"),
        &format!("[package]\nname = \"nat\"\nversion = \"0.1.0\"\nkind = \"native-plugin\"\n[native]\nsupported_os = [\"plan9\"]\nsupported_arch = [\"{}\"]\nabi_version = 1\n", arch),
    );
    let r = flashpkg(&app, &["add", "nat", "--path", "../nat"]);
    assert_eq!(r.status.code(), Some(1));
    assert!(text(&r.stderr).contains("supports [\"plan9\"]"), "{}", text(&r.stderr));
    // wrong ABI version
    write(
        base.join("nat/flash.toml"),
        &format!("[package]\nname = \"nat\"\nversion = \"0.1.0\"\nkind = \"native-plugin\"\n[native]\nsupported_os = [\"{}\"]\nsupported_arch = [\"{}\"]\nabi_version = 7\n", os, arch),
    );
    let r = flashpkg(&app, &["add", "nat", "--path", "../nat"]);
    assert_eq!(r.status.code(), Some(1));
    assert!(text(&r.stderr).contains("needs plugin ABI version 7"), "{}", text(&r.stderr));
    // compatible, with a build command that must NOT run without --allow-native-builds
    let marker = base.join("build_ran");
    write(
        base.join("nat/flash.toml"),
        &format!(
            "[package]\nname = \"nat\"\nversion = \"0.1.0\"\nkind = \"native-plugin\"\n[native]\nsupported_os = [\"{}\"]\nsupported_arch = [\"{}\"]\nabi_version = 1\nbuild = \"touch {}\"\n",
            os,
            arch,
            marker.display()
        ),
    );
    let r = flashpkg(&app, &["add", "nat", "--path", "../nat"]);
    assert!(r.status.success(), "{}", text(&r.stderr));
    let b = flashpkg(&app, &["build"]);
    assert!(b.status.success(), "{}", text(&b.stderr));
    assert!(!marker.exists(), "a dependency's build command must not run implicitly");
    assert!(text(&b.stderr).contains("not run"));
    let b = flashpkg(&app, &["build", "--allow-native-builds"]);
    assert!(b.status.success(), "{}", text(&b.stderr));
    assert!(marker.exists(), "build command should run when explicitly allowed");
}

#[test]
fn rust_plugin_package_from_the_repository_works_through_flashpkg() {
    let root = repo_root();
    let st = Command::new("cargo").current_dir(&root).args(["build", "-p", "fast_math", "--release", "--quiet"]).status().unwrap();
    assert!(st.success());
    let base = scratch_dir("pkg-plugin");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    assert!(flashpkg(&base, &["init", "app"]).status.success());
    let app = base.join("app");
    let plug = root.join("plugins/rust/fast_math");
    let r = flashpkg(&app, &["add", "fast_math", "--path", plug.to_str().unwrap()]);
    assert!(r.status.success(), "{}", text(&r.stderr));
    write(app.join("src/main.flash"), "use rust_plugin \"fast_math\"\nsay(fast_math.add(20, 22))\n");
    let run = flashpkg(&app, &["run"]);
    assert!(run.status.success(), "{}", text(&run.stderr));
    assert!(text(&run.stdout).contains("42"), "{}", text(&run.stdout));
}
