//! Runs the ownership-heavy programs under Valgrind (when installed): no invalid reads or writes,
//! no use of freed memory, no double frees and no leaked blocks. This checks the *generated
//! code*, independently of the runtime's own allocation counter.

mod common;
use common::*;
use std::process::Command;

fn valgrind_available() -> bool {
    Command::new("valgrind").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

#[test]
fn ownership_programs_are_clean_under_valgrind() {
    if !valgrind_available() {
        eprintln!("note: valgrind not installed; skipping the memory check");
        return;
    }
    let root = repo_root();
    let dir = scratch_dir("memcheck");
    let mut configs: Vec<Vec<&str>> = vec![vec![], vec!["--release"]];
    if clang_available() {
        configs.push(vec!["--release", "--backend", "llvm"]);
    }
    for prog in [
        "tests/programs/ownership.flash",
        "tests/programs/ownership_flow.flash",
        "tests/programs/strings.flash",
        "tests/programs/modules.flash",
        "examples/lists.flash",
    ] {
        for (i, cfg) in configs.iter().enumerate() {
            let exe = dir.join(format!("vg{}", i));
            let src = root.join(prog);
            let mut args = vec!["build", src.to_str().unwrap(), "-o", exe.to_str().unwrap(), "--no-cache"];
            args.extend(cfg.iter());
            let b = flash(&args);
            assert!(b.status.success(), "{}", text(&b.stderr));
            let v = Command::new("valgrind")
                .args(["--error-exitcode=9", "--leak-check=full", "--errors-for-leak-kinds=definite,indirect", "-q"])
                .arg(&exe)
                .output()
                .unwrap();
            assert_eq!(v.status.code(), Some(0), "{} {:?}: valgrind reported problems:\n{}", prog, cfg, text(&v.stderr));
        }
    }
}
