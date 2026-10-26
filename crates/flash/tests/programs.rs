//! Compiles every program in tests/programs and examples/ in dev and release mode, runs it, and
//! checks stdout, exit code, and that all heap memory was released (no leaks).

mod common;
use common::*;

fn run_program(dir: &str, name: &str) {
    let root = repo_root();
    let src = root.join(dir).join(format!("{}.flash", name));
    let expected = std::fs::read_to_string(root.join(dir).join(format!("{}.expected", name))).unwrap();
    let want_exit: i32 =
        std::fs::read_to_string(root.join(dir).join(format!("{}.exit", name))).map(|s| s.trim().parse().unwrap()).unwrap_or(0);
    // dev and release with the native back end; with `--features legacy-backends` also the
    // Cranelift back end (system linker) and, when clang is installed, LLVM.
    let mut configs: Vec<(bool, Option<&str>, &[&str])> = vec![(false, None, &[]), (true, None, &[])];
    if cfg!(feature = "legacy-backends") {
        configs.push((false, None, &["--linker", "system"]));
        configs.push((true, None, &["--linker", "system"]));
        if clang_available() {
            configs.push((true, Some("llvm"), &[]));
        } else {
            eprintln!("note: clang not found, skipping the LLVM backend for {}", name);
        }
    }
    for (release, backend, extra) in configs {
        let mode = format!(
            "{}{}{}",
            if release { "release" } else { "dev" },
            backend.map(|b| format!("-{}", b)).unwrap_or_default(),
            if extra.is_empty() { "" } else { "-syslink" }
        );
        let (b, r) = build_and_run_extra(&src, release, backend, extra, &format!("{}-{}", name, mode), &[]);
        assert!(b.status.success(), "{} ({}) failed to compile:\n{}", name, mode, text(&b.stderr));
        let r = r.unwrap();
        assert_eq!(text(&r.stdout), expected, "{} ({}): wrong output", name, mode);
        assert_eq!(r.status.code(), Some(want_exit), "{} ({}): wrong exit code; stderr: {}", name, mode, text(&r.stderr));
        if want_exit == 0 {
            assert!(
                text(&r.stderr).contains("live allocations at exit: 0"),
                "{} ({}): memory leak or missing leak report: {}",
                name,
                mode,
                text(&r.stderr)
            );
        }
    }
}

macro_rules! programs {
    ($dir:literal: $($name:ident),*) => {
        $( #[test] fn $name() { run_program($dir, stringify!($name)); } )*
    };
}

mod tests_dir {
    use super::*;
    programs!("tests/programs": integers, const_division, methods, tuples, strings, scopes, results, ownership, ownership_flow, generics, modules, tree);
}

mod examples_dir {
    use super::*;
    programs!("examples": hello, arithmetic, conditions, loops, functions, structs, lists);
}
