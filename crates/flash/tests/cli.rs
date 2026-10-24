//! Command-line behaviour: exit codes, output files, formatter, test runner, cross-compilation.

mod common;
use common::*;
use std::process::Command;

#[test]
fn help_and_version_succeed_and_unknown_command_is_a_usage_error() {
    assert_eq!(flash(&["--help"]).status.code(), Some(0));
    assert_eq!(flash(&[]).status.code(), Some(0));
    let v = flash(&["--version"]);
    assert_eq!(v.status.code(), Some(0));
    assert!(text(&v.stdout).starts_with("flash 0.1"));
    assert_eq!(flash(&["frobnicate"]).status.code(), Some(2));
    assert_eq!(flash(&["run"]).status.code(), Some(2)); // missing file
    assert_eq!(flash(&["build", "x.flash", "--bogus"]).status.code(), Some(2));
}

#[test]
fn check_reports_ok_or_errors_with_exit_codes() {
    let root = repo_root();
    let ok = flash(&["check", root.join("examples/hello.flash").to_str().unwrap()]);
    assert_eq!(ok.status.code(), Some(0));
    assert!(text(&ok.stdout).contains("no errors"));
    let bad = flash(&["check", root.join("tests/errors/add_int_string.flash").to_str().unwrap()]);
    assert_eq!(bad.status.code(), Some(1));
    let msg = text(&bad.stderr);
    assert!(msg.contains("Cannot add Int and String."), "{}", msg);
    assert!(msg.contains("line 2"), "{}", msg);
    let missing = flash(&["check", "/no/such/file.flash"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(text(&missing.stderr).contains("cannot read"));
}

#[test]
fn run_forwards_the_program_exit_code_and_arguments() {
    let dir = scratch_dir("cli-run");
    let src = dir.join("args.flash");
    std::fs::write(
        &src,
        "a = args()\nsay(len(a))\nfor x in a:\n    say(x)\nif len(a) > 1:\n    exit(7)\n",
    )
    .unwrap();
    let out = flash(&["run", src.to_str().unwrap(), "--", "one", "two"]);
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(text(&out.stdout), "2\none\ntwo\n");
    let out = flash(&["run", src.to_str().unwrap(), "--", "solo"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "1\nsolo\n");
}

#[test]
fn build_produces_a_standalone_native_executable() {
    let dir = scratch_dir("cli-build");
    let exe = dir.join("hello");
    let out = flash(&["build", repo_root().join("examples/hello.flash").to_str().unwrap(), "-o", exe.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let bytes = std::fs::read(&exe).unwrap();
    // a native executable, not a script or interpreter payload
    #[cfg(target_os = "linux")]
    assert_eq!(&bytes[..4], b"\x7fELF");
    #[cfg(target_os = "macos")]
    assert!(bytes[..4] == [0xcf, 0xfa, 0xed, 0xfe] || bytes[..4] == [0xca, 0xfe, 0xba, 0xbe]);
    assert!(bytes.len() > 1000);
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(text(&run.stdout), "Hello, world!\n");
    // does not need the Flash compiler or the source file
    std::fs::remove_file(dir.join("hello.flash.o")).ok();
}

#[test]
fn release_and_dev_builds_agree_but_release_is_not_slower_to_run() {
    let root = repo_root();
    let src = root.join("tests/programs/generics.flash");
    let (_, dev) = build_and_run(&src, false, "cli-dev", &[]);
    let (_, rel) = build_and_run(&src, true, "cli-rel", &[]);
    assert_eq!(text(&dev.unwrap().stdout), text(&rel.unwrap().stdout));
}

#[test]
fn runtime_errors_exit_with_code_101_and_a_message() {
    let dir = scratch_dir("cli-panic");
    for (code, msg, needle) in [
        ("xs = [1, 2]\ni = 5\nsay(xs[i])\n", "out of range", "index 5 is out of range for a list of length 2"),
        ("a = 10\nb = a - 10\nsay(a / b)\n", "division", "division by zero"),
        ("a = 10\nb = a - 10\nsay(a % b)\n", "remainder", "division by zero"),
        ("xs: List[Int] = []\nsay(pop(xs))\n", "pop", "pop() from an empty list"),
        ("assert(1 > 2, \"nope\")\n", "assert", "Assertion failed: nope"),
        ("say(unwrap(parse_int(\"x\")))\n", "unwrap", "unwrap() on an error"),
    ] {
        let src = dir.join("p.flash");
        std::fs::write(&src, code).unwrap();
        let out = flash(&["run", src.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(101), "{}: {}", msg, text(&out.stderr));
        assert!(text(&out.stderr).contains(needle), "{}: {}", msg, text(&out.stderr));
    }
}

#[test]
fn formatter_is_idempotent_preserves_meaning_and_check_mode_works() {
    let root = repo_root();
    let ugly = "# top\nfn   add( a,b ):\n  return a+b   # sum\n\n\n\nx=add(1,2)*-3\nprint( f\"v={x}\" , 'q')\nsay()\n";
    let dir = scratch_dir("cli-fmt");
    let f = dir.join("ugly.flash");
    std::fs::write(&f, ugly).unwrap();
    let before = flash(&["run", f.to_str().unwrap()]);
    let out = flash(&["fmt", f.to_str().unwrap(), "--stdout"]);
    let formatted = text(&out.stdout);
    assert_eq!(
        formatted,
        "# top\nfn add(a, b):\n    return a + b  # sum\n\n\nx = add(1, 2) * -3\nprint(f\"v={x}\", 'q')\nsay()\n"
    );
    assert_eq!(flash(&["fmt", f.to_str().unwrap(), "--check"]).status.code(), Some(1));
    assert_eq!(flash(&["fmt", f.to_str().unwrap()]).status.code(), Some(0));
    assert_eq!(flash(&["fmt", f.to_str().unwrap(), "--check"]).status.code(), Some(0));
    let after = flash(&["run", f.to_str().unwrap()]);
    assert_eq!(text(&before.stdout), text(&after.stdout));
    // every repository program: formatting is idempotent and does not change behaviour
    for dir in ["examples", "tests/programs", "stdlib"] {
        for e in std::fs::read_dir(root.join(dir)).unwrap() {
            let p = e.unwrap().path();
            if p.extension().and_then(|e| e.to_str()) != Some("flash") {
                continue;
            }
            let once = text(&flash(&["fmt", p.to_str().unwrap(), "--stdout"]).stdout);
            let tmp = dir_tmp(&once);
            let twice = text(&flash(&["fmt", tmp.to_str().unwrap(), "--stdout"]).stdout);
            assert_eq!(once, twice, "formatting {} is not idempotent", p.display());
            assert_eq!(once, std::fs::read_to_string(&p).unwrap(), "{} is not formatted; run `flash fmt`", p.display());
        }
    }
}

fn dir_tmp(content: &str) -> std::path::PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let d = scratch_dir(&format!("fmt-tmp-{}", N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
    let p = d.join("t.flash");
    std::fs::write(&p, content).unwrap();
    p
}

#[test]
fn flash_test_runs_test_functions_and_reports_failures() {
    let root = repo_root();
    let ok = flash(&["test", root.join("tests/flash").to_str().unwrap()]);
    assert_eq!(ok.status.code(), Some(0), "{}{}", text(&ok.stdout), text(&ok.stderr));
    assert!(text(&ok.stdout).contains("5 passed; 0 failed"), "{}", text(&ok.stdout));
    let dir = scratch_dir("cli-test");
    let f = dir.join("bad_test.flash");
    std::fs::write(&f, "fn test_good():\n    assert(True)\nfn test_bad():\n    assert(1 == 2, \"one is not two\")\n").unwrap();
    let bad = flash(&["test", f.to_str().unwrap()]);
    assert_eq!(bad.status.code(), Some(1));
    let out = text(&bad.stdout);
    assert!(out.contains("test_good ... ok") && out.contains("test_bad ... FAILED") && out.contains("one is not two"), "{}", out);
    assert!(out.contains("1 passed; 1 failed"));
}

#[test]
fn cross_compilation_emits_objects_for_other_platforms() {
    // Linking needs a target toolchain, but the code generator can produce objects for each target.
    let src = repo_root().join("examples/functions.flash");
    let dir = scratch_dir("cli-targets");
    let cases: [(&str, &[u8]); 4] = [
        ("aarch64-apple-darwin", &[0xcf, 0xfa, 0xed, 0xfe]),  // Mach-O 64-bit little endian
        ("x86_64-apple-darwin", &[0xcf, 0xfa, 0xed, 0xfe]),
        ("aarch64-unknown-linux-gnu", b"\x7fELF"),
        ("x86_64-pc-windows-msvc", &[0x64, 0x86]),            // COFF machine type AMD64
    ];
    for (target, magic) in cases {
        for release in [false, true] {
            let out = dir.join(format!("{}-{}.o", target, release));
            let mut args = vec!["build", src.to_str().unwrap(), "--target", target, "-o", out.to_str().unwrap()];
            if release {
                args.push("--release");
            }
            let r = flash(&args);
            assert!(r.status.success(), "{}: {}", target, text(&r.stderr));
            let bytes = std::fs::read(&out).unwrap();
            assert_eq!(&bytes[..magic.len()], magic, "{} has the wrong object format", target);
            if target == "aarch64-apple-darwin" {
                // cputype ARM64 = 0x0100000c
                assert_eq!(&bytes[4..8], &[0x0c, 0x00, 0x00, 0x01]);
            }
            if target == "aarch64-unknown-linux-gnu" {
                assert_eq!(&bytes[18..20], &[0xb7, 0x00]); // e_machine = EM_AARCH64
            }
        }
    }
    let bad = flash(&["build", src.to_str().unwrap(), "--target", "not-a-target", "-o", dir.join("x.o").to_str().unwrap()]);
    assert_eq!(bad.status.code(), Some(1));
}
