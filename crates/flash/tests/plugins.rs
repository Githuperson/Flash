//! Plugin integration: real native calls into a Rust cdylib and a C++ shared library, plus ABI
//! validation failures. These tests compile native code with `cargo` and `c++`.

mod common;
use common::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn cxx() -> String {
    std::env::var("CXX").unwrap_or_else(|_| "c++".into())
}

fn lib_ext() -> &'static str {
    if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(windows) {
        "dll"
    } else {
        "so"
    }
}

fn lib_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{}.{}", base, lib_ext())
    } else {
        format!("lib{}.{}", base, lib_ext())
    }
}

/// Build the two example plugins once; returns the directories that contain the libraries.
fn example_plugin_dirs() -> &'static Vec<PathBuf> {
    static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DIRS.get_or_init(|| {
        let root = repo_root();
        let st = Command::new("cargo").current_dir(&root).args(["build", "-p", "fast_math", "--quiet"]).status().unwrap();
        assert!(st.success(), "building the Rust example plugin failed");
        let target = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| root.join("target"));
        let rust_dir = target.join("debug");
        assert!(rust_dir.join(lib_name("fast_math")).is_file(), "Rust plugin library missing");
        let out = scratch_dir("cpp-geometry");
        let lib = out.join(lib_name("geometry"));
        let st = Command::new(cxx())
            .args(["-std=c++17", "-O1", "-fPIC", "-fvisibility=hidden", "-shared"])
            .arg(format!("-I{}", root.join("include").display()))
            .arg(root.join("plugins/cpp/geometry/geometry.cpp"))
            .arg("-o")
            .arg(&lib)
            .status()
            .expect("cannot run the C++ compiler");
        assert!(st.success(), "building the C++ example plugin failed");
        vec![rust_dir, out]
    })
}

fn plugin_path_env(extra: &[&Path]) -> std::ffi::OsString {
    let mut v: Vec<PathBuf> = example_plugin_dirs().clone();
    // manifests of the examples live next to their sources
    v.push(repo_root().join("plugins/rust/fast_math"));
    v.push(repo_root().join("plugins/cpp/geometry"));
    v.extend(extra.iter().map(|p| p.to_path_buf()));
    std::env::join_paths(v).unwrap()
}

fn run_flash_with_plugins(src: &Path, extra: &[&Path], tag: &str) -> (std::process::Output, Option<std::process::Output>) {
    let dir = scratch_dir(tag);
    let exe = dir.join("prog");
    let b = Command::new(flash_bin())
        .args(["build", src.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .env("FLASH_PLUGIN_PATH", plugin_path_env(extra))
        .output()
        .unwrap();
    if !b.status.success() {
        return (b, None);
    }
    let r = Command::new(&exe).env("FLASH_PLUGIN_PATH", plugin_path_env(extra)).env("FLASH_RT_LEAKCHECK", "1").output().unwrap();
    (b, Some(r))
}

#[test]
fn rust_and_cpp_plugins_are_called_end_to_end() {
    let root = repo_root();
    let (b, r) = run_flash_with_plugins(&root.join("examples/plugins_demo.flash"), &[], "demo");
    assert!(b.status.success(), "compile failed: {}", text(&b.stderr));
    let r = r.unwrap();
    let expected = std::fs::read_to_string(root.join("examples/plugins_demo.expected")).unwrap();
    assert_eq!(text(&r.stdout), expected, "stderr: {}", text(&r.stderr));
    assert!(r.status.success());
    let err = text(&r.stderr);
    // provenance + trust warning, memory released, panic/exception did not escape
    assert!(err.contains("loaded native plugin 'fast_math'") && err.contains("only use plugins you trust"), "{}", err);
    assert!(err.contains("loaded native plugin 'geometry'"));
    assert!(err.contains("live allocations at exit: 0"), "{}", err);
}

#[test]
fn plugin_calls_work_in_release_mode_too() {
    let root = repo_root();
    let dir = scratch_dir("demo-rel");
    let exe = dir.join("prog");
    let b = Command::new(flash_bin())
        .args(["build", root.join("examples/plugins_demo.flash").to_str().unwrap(), "-o", exe.to_str().unwrap(), "--release"])
        .env("FLASH_PLUGIN_PATH", plugin_path_env(&[]))
        .output()
        .unwrap();
    assert!(b.status.success(), "{}", text(&b.stderr));
    let r = Command::new(&exe).env("FLASH_PLUGIN_PATH", plugin_path_env(&[])).env("FLASH_PLUGIN_QUIET", "1").output().unwrap();
    let expected = std::fs::read_to_string(root.join("examples/plugins_demo.expected")).unwrap();
    assert_eq!(text(&r.stdout), expected);
}

// ---------------------------------------------------------------- ABI validation

struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        Scratch { dir: scratch_dir(tag) }
    }
    fn write(&self, name: &str, content: &str) -> PathBuf {
        let p = self.dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }
    /// Compile a C plugin source against the Flash header.
    fn build_c_plugin(&self, base: &str, src: &str) {
        let c = self.write(&format!("{}.c", base), src);
        let out = self.dir.join(lib_name(base));
        let st = Command::new("cc")
            .args(["-fPIC", "-shared", "-O1"])
            .arg(format!("-I{}", repo_root().join("include").display()))
            .arg(&c)
            .arg("-o")
            .arg(&out)
            .status()
            .expect("cannot run cc");
        assert!(st.success());
    }
    fn manifest(&self, name: &str, abi: u32, kind: &str, funcs: &str) {
        self.write(
            &format!("{}.flashplugin.toml", name),
            &format!("[plugin]\nname = \"{}\"\nkind = \"{}\"\nabi_version = {}\nlibrary = \"{}\"\n{}", name, kind, abi, name, funcs),
        );
    }
    /// Compile a Flash program using the plugin; returns (compile ok, compile stderr, run output).
    fn run(&self, program: &str) -> (bool, String, Option<std::process::Output>) {
        let src = self.write("main.flash", program);
        let exe = self.dir.join("main_exe");
        let env = std::env::join_paths([self.dir.clone()]).unwrap();
        let b = Command::new(flash_bin())
            .args(["build", src.to_str().unwrap(), "-o", exe.to_str().unwrap()])
            .env("FLASH_PLUGIN_PATH", &env)
            .output()
            .unwrap();
        if !b.status.success() {
            return (false, text(&b.stderr), None);
        }
        let r = Command::new(&exe).env("FLASH_PLUGIN_PATH", &env).env("FLASH_PLUGIN_QUIET", "1").output().unwrap();
        (true, text(&b.stderr), Some(r))
    }
}

const GOOD_C: &str = r#"
#include "flash_plugin.h"
static int32_t twice(const FlashValue* a, uint32_t n, FlashValue* r) { flash_set_i64(r, (int64_t)a[0].a * 2); return 0; }
static void fb(uint8_t* p, uint64_t l) {}
static const uint32_t P[] = {FLASH_T_I64};
static const FlashFuncInfo F[] = {{"twice", 1, FLASH_T_I64, P, twice}};
static const FlashPluginInfo I = {ABI, sizeof(FlashPluginInfo), "NAME", "1.0", 1, F, fb};
FLASH_EXPORT const FlashPluginInfo* flash_plugin_entry_v1(void) { return &I; }
"#;

fn good_c(name: &str, abi: u32) -> String {
    GOOD_C.replace("NAME", name).replace("ABI", &abi.to_string())
}

const TWICE_FN: &str = "\n[[function]]\nname = \"twice\"\nparams = [\"Int\"]\nreturns = \"Int\"\n";
const USE_TWICE: &str = "use cpp_plugin \"p\"\nsay(p.twice(21))\n";

#[test]
fn a_minimal_c_plugin_works() {
    let s = Scratch::new("abi-good");
    s.build_c_plugin("p", &good_c("p", 1));
    s.manifest("p", 1, "cpp", TWICE_FN);
    let (ok, err, r) = s.run(USE_TWICE);
    assert!(ok, "{}", err);
    assert_eq!(text(&r.unwrap().stdout), "42\n");
}

#[test]
fn manifest_with_unsupported_abi_version_is_rejected_at_compile_time() {
    let s = Scratch::new("abi-manifest");
    s.build_c_plugin("p", &good_c("p", 1));
    s.manifest("p", 2, "cpp", TWICE_FN);
    let (ok, err, _) = s.run(USE_TWICE);
    assert!(!ok);
    assert!(err.contains("requires plugin ABI version 2"), "{}", err);
}

#[test]
fn plugin_reporting_a_different_abi_version_is_rejected_at_load_time() {
    let s = Scratch::new("abi-lib");
    s.build_c_plugin("p", &good_c("p", 99));
    s.manifest("p", 1, "cpp", TWICE_FN);
    let (ok, err, r) = s.run(USE_TWICE);
    assert!(ok, "{}", err);
    let r = r.unwrap();
    assert_eq!(r.status.code(), Some(1));
    assert!(text(&r.stderr).contains("reports ABI version 99"), "{}", text(&r.stderr));
    assert_eq!(text(&r.stdout), "");
}

#[test]
fn missing_function_signature_mismatch_and_wrong_name_are_detected() {
    // function in manifest that the library does not export
    let s = Scratch::new("abi-missing");
    s.build_c_plugin("p", &good_c("p", 1));
    s.manifest("p", 1, "cpp", &format!("{}\n[[function]]\nname = \"nope\"\nreturns = \"Int\"\n", TWICE_FN));
    let (_, _, r) = s.run("use cpp_plugin \"p\"\nsay(p.nope())\n");
    let r = r.unwrap();
    assert_eq!(r.status.code(), Some(1));
    assert!(text(&r.stderr).contains("does not export the function 'nope'"), "{}", text(&r.stderr));

    // manifest says Float, library says Int
    let s = Scratch::new("abi-sig");
    s.build_c_plugin("p", &good_c("p", 1));
    s.manifest("p", 1, "cpp", "\n[[function]]\nname = \"twice\"\nparams = [\"Float\"]\nreturns = \"Int\"\n");
    let (_, _, r) = s.run("use cpp_plugin \"p\"\nsay(p.twice(2.0))\n");
    let r = r.unwrap();
    assert_eq!(r.status.code(), Some(1));
    assert!(text(&r.stderr).contains("does not match its manifest"), "{}", text(&r.stderr));

    // library identifies as another plugin
    let s = Scratch::new("abi-name");
    s.build_c_plugin("p", &good_c("other", 1));
    s.manifest("p", 1, "cpp", TWICE_FN);
    let (_, _, r) = s.run(USE_TWICE);
    let r = r.unwrap();
    assert_eq!(r.status.code(), Some(1));
    assert!(text(&r.stderr).contains("identifies itself as plugin 'other'"), "{}", text(&r.stderr));
}

#[test]
fn library_without_entry_point_or_missing_library_is_reported() {
    let s = Scratch::new("abi-noentry");
    s.build_c_plugin("p", "int unrelated(void) { return 1; }\n");
    s.manifest("p", 1, "cpp", TWICE_FN);
    let (_, _, r) = s.run(USE_TWICE);
    let r = r.unwrap();
    assert_eq!(r.status.code(), Some(1));
    assert!(text(&r.stderr).contains("does not export the entry point 'flash_plugin_entry_v1'"), "{}", text(&r.stderr));

    let s = Scratch::new("abi-nolib");
    s.manifest("p", 1, "cpp", TWICE_FN);
    let (_, _, r) = s.run(USE_TWICE);
    let r = r.unwrap();
    assert_eq!(r.status.code(), Some(1));
    assert!(text(&r.stderr).contains("cannot find the plugin library"), "{}", text(&r.stderr));
}

#[test]
fn integrity_pin_is_enforced() {
    use std::fmt::Write;
    let s = Scratch::new("abi-integrity");
    s.build_c_plugin("p", &good_c("p", 1));
    // compute sha256 of the library with the system tool-free approach: ask the runtime to tell us
    s.manifest("p", 1, "cpp", TWICE_FN);
    let wrong = "0".repeat(64);
    let text_m = std::fs::read_to_string(s.dir.join("p.flashplugin.toml")).unwrap();
    let mut with_pin = text_m.replace("library = \"p\"\n", &format!("library = \"p\"\nintegrity = \"sha256:{}\"\n", wrong));
    let (_, _, r) = s.run(USE_TWICE);
    // first run (no pin) succeeds and prints the digest when not quiet; get it from the warning
    let exe = s.dir.join("main_exe");
    let out = Command::new(&exe).env("FLASH_PLUGIN_PATH", s.dir.clone()).output().unwrap();
    assert_eq!(text(&r.unwrap().stdout), "42\n");
    assert!(text(&out.stderr).contains("sha256"), "{}", text(&out.stderr));
    // wrong pin
    std::fs::write(s.dir.join("p.flashplugin.toml"), &with_pin).unwrap();
    let (_, _, r) = s.run(USE_TWICE);
    let r = r.unwrap();
    assert_eq!(r.status.code(), Some(1));
    assert!(text(&r.stderr).contains("integrity check failed"), "{}", text(&r.stderr));
    with_pin.clear();
    let _ = write!(with_pin, "");
}

#[test]
fn use_keyword_must_match_plugin_kind() {
    let s = Scratch::new("abi-kind");
    s.build_c_plugin("p", &good_c("p", 1));
    s.manifest("p", 1, "cpp", TWICE_FN);
    let (ok, err, _) = s.run("use rust_plugin \"p\"\nsay(p.twice(1))\n");
    assert!(!ok);
    assert!(err.contains("declares kind \"cpp\""), "{}", err);
}

#[test]
fn plugin_calls_are_type_checked_at_compile_time() {
    let s = Scratch::new("abi-typecheck");
    s.build_c_plugin("p", &good_c("p", 1));
    s.manifest("p", 1, "cpp", TWICE_FN);
    let (ok, err, _) = s.run("use cpp_plugin \"p\"\nsay(p.twice(\"x\"))\n");
    assert!(!ok);
    assert!(err.contains("should be Int, but this is String"), "{}", err);
    let (ok, err, _) = s.run("use cpp_plugin \"p\"\nsay(p.twic(1))\n");
    assert!(!ok);
    assert!(err.contains("does not export a function 'twic'") && err.contains("Did you mean 'twice'?"), "{}", err);
    let (ok, err, _) = s.run("use cpp_plugin \"missing\"\n");
    assert!(!ok);
    assert!(err.contains("Cannot find the plugin manifest"), "{}", err);
}
