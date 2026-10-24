//! Static linking: `extern fn` declarations resolved against C, C++ and Rust static libraries.

mod common;
use common::*;
use std::process::Command;

fn build_libs(tag: &str) -> std::path::PathBuf {
    let dir = scratch_dir(tag);
    let src = repo_root().join("examples/static_link");
    // built into a scratch directory, not into the source tree
    let cc = |args: &[&str]| {
        let st = Command::new(args[0]).args(&args[1..]).status().expect("tool not found");
        assert!(st.success(), "{:?} failed", args);
    };
    let d = dir.to_str().unwrap();
    cc(&["cc", "-O2", "-fPIC", "-c", src.join("mathlib.c").to_str().unwrap(), "-o", &format!("{}/mathlib.o", d)]);
    cc(&["ar", "rcs", &format!("{}/libmathlib.a", d), &format!("{}/mathlib.o", d)]);
    cc(&["c++", "-std=c++17", "-O2", "-fPIC", "-c", src.join("stats.cpp").to_str().unwrap(), "-o", &format!("{}/stats.o", d)]);
    cc(&["ar", "rcs", &format!("{}/libstats.a", d), &format!("{}/stats.o", d)]);
    cc(&[
        "rustc",
        "--crate-type",
        "staticlib",
        "-C",
        "panic=abort",
        "-C",
        "opt-level=2",
        "--edition",
        "2021",
        src.join("rust_static.rs").to_str().unwrap(),
        "-o",
        &format!("{}/librust_static.a", d),
    ]);
    dir
}

fn run(src: &std::path::Path, extra: &[&str], tag: &str) -> (std::process::Output, Option<std::process::Output>) {
    let dir = scratch_dir(tag);
    let exe = dir.join("prog");
    let mut args = vec!["build", src.to_str().unwrap(), "-o", exe.to_str().unwrap()];
    args.extend_from_slice(extra);
    let b = flash(&args);
    if !b.status.success() {
        return (b, None);
    }
    (b, Some(Command::new(&exe).env("FLASH_RT_LEAKCHECK", "1").output().unwrap()))
}

#[test]
fn c_cpp_and_rust_static_libraries_link_and_run() {
    let libs = build_libs("static-libs");
    let l = |n: &str| libs.join(n).to_str().unwrap().to_string();
    let expected = std::fs::read_to_string(repo_root().join("examples/static_link/main.expected")).unwrap();
    for (release, backend) in [(false, None), (true, None), (true, Some("llvm"))] {
        if backend.is_some() && !clang_available() {
            continue;
        }
        let (m, s, r) = (l("libmathlib.a"), l("libstats.a"), l("librust_static.a"));
        let mut extra = vec!["--link", m.as_str(), "--link", s.as_str(), "-lstdc++"];
        if release {
            extra.push("--release");
        }
        if let Some(b) = backend {
            extra.extend(["--backend", b]);
        }
        let (b, run_out) = run(&repo_root().join("examples/static_link/main.flash"), &extra, "static-run");
        assert!(b.status.success(), "{}", text(&b.stderr));
        let o = run_out.unwrap();
        assert_eq!(text(&o.stdout), expected, "release={} backend={:?}", release, backend);
        assert!(text(&o.stderr).contains("live allocations at exit: 0"));
        // the no_std Rust static library
        let dir = scratch_dir("static-rs");
        let p = dir.join("rs.flash");
        std::fs::write(&p, "extern fn rs_square(x: Int) -> Int\nextern fn rs_clamp(x: Int, lo: Int, hi: Int) -> Int\nsay(rs_square(12), rs_clamp(50, 0, 10), rs_clamp(-5, 0, 10))\n").unwrap();
        let (b, o) = run(&p, &["--link", r.as_str(), "-Wl,--allow-multiple-definition"], "static-rs-run");
        assert!(b.status.success(), "{}", text(&b.stderr));
        assert_eq!(text(&o.unwrap().stdout), "144 10 0\n");
    }
}

#[test]
fn extern_functions_are_type_checked_and_missing_libraries_are_explained() {
    let dir = scratch_dir("static-errs");
    let write = |name: &str, code: &str| {
        let p = dir.join(name);
        std::fs::write(&p, code).unwrap();
        p
    };
    let check = |code: &str| {
        let p = write("e.flash", code);
        text(&flash(&["check", p.to_str().unwrap()]).stderr)
    };
    assert!(check("extern fn f(x: List[Int]) -> Int\n").contains("An `extern fn` parameter cannot have type List[Int]"));
    assert!(check("extern fn f(x: Int) -> String\n").contains("cannot return String"));
    assert!(check("extern fn fl_secret(x: Int) -> Int\n").contains("reserved for the Flash runtime"));
    assert!(check("extern fn f(x: Int) -> Int\nsay(f(\"a\"))\n").contains("should be Int, but this is String"));
    assert!(check("extern fn f(x: Int) -> Int\nsay(f(1, 2))\n").contains("takes 1 argument"));
    assert!(check("extern fn f(x: u8) -> Int\nsay(f(300))\n").contains("does not fit in u8"));
    assert_eq!(check("extern fn f(x: u8) -> Int\nx: Int = f(200)\n"), "");
    // type checks pass but the symbol does not exist: the linker error comes with a hint
    let p = write("missing.flash", "extern fn definitely_not_defined_anywhere(x: Int) -> Int\nsay(definitely_not_defined_anywhere(1))\n");
    let out = flash(&["build", p.to_str().unwrap(), "-o", dir.join("m").to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.contains("linking failed") && err.contains("Pass the library that defines it"), "{}", err);
}
