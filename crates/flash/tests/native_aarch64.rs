//! The AArch64 executables (Linux ELF and macOS Mach-O), run in a CPU emulator (scripts/run_a64.py, which needs python3
//! and the `unicorn` package; the tests skip themselves when it is missing). Every program must
//! print exactly what the x86-64 build prints.

mod common;
use common::*;
use std::path::Path;
use std::process::{Command, Stdio};

fn emulator_available() -> bool {
    Command::new("python3")
        .args(["-c", "import unicorn"])
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

const TARGETS: [&str; 2] = ["aarch64-unknown-linux-gnu", "aarch64-apple-darwin"];

/// Build `src` for `target` and run it in the emulator; returns stdout.
fn emulate_for(src: &Path, release: bool, target: &str, tag: &str) -> String {
    let exe = scratch_dir(tag).join("prog.a64");
    let mut args = vec!["build", src.to_str().unwrap(), "-o", exe.to_str().unwrap(), "--target", target];
    if release {
        args.push("--release");
    }
    let b = flash(&args);
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    let r = Command::new("python3").arg(repo_root().join("scripts/run_a64.py")).arg(&exe).output().unwrap();
    assert!(r.status.success(), "{}{}", String::from_utf8_lossy(&r.stdout), String::from_utf8_lossy(&r.stderr));
    String::from_utf8_lossy(&r.stdout).into_owned()
}

#[test]
fn examples_run_on_aarch64() {
    if !emulator_available() {
        eprintln!("skipped: python3 with unicorn is not available");
        return;
    }
    for name in ["hello", "arithmetic", "conditions", "functions", "lists", "loops", "structs"] {
        let src = repo_root().join("examples").join(format!("{}.flash", name));
        let expected = std::fs::read_to_string(src.with_extension("expected")).unwrap();
        for target in TARGETS {
            for release in [false, true] {
                assert_eq!(emulate_for(&src, release, target, &format!("a64-{}-{}", name, release)), expected, "{} {} (release: {})", name, target, release);
            }
        }
    }
}

const FLOATS: &str = r#"
text = unwrap(read_file("INPUT"))
for line in split(text, "\n"):
    if len(line) > 0:
        x = unwrap(parse_float(line))
        say(x, sin(x), cos(x), tan(x), exp(x), ln(x), pow(x, 0.75), pow(2.5, x), sqrt(x), floor(x), ceil(x))
        say(int(x * 1000.0) % 977, int(x) / 7, int(x) % 13)
"#;

#[test]
fn float_text_and_math_match_x86_64_on_aarch64() {
    if !emulator_available() {
        eprintln!("skipped: python3 with unicorn is not available");
        return;
    }
    let dir = scratch_dir("a64-floats");
    let mut state = 0x9E3779B97F4A7C15u64;
    let mut next = || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545F4914F6CDD1D)
    };
    let mut text = String::from("0\n1\n0.5\n2.5\n1e22\n1e-7\n123456789.125\n4.9e-324\n1.7976931348623157e308\n0.1\n3.14159265358979\n100\n");
    for _ in 0..150 {
        let v = f64::from_bits(next());
        if v.is_finite() && v.abs() < 1e30 {
            text.push_str(&format!("{:e}\n", v.abs()));
        }
        text.push_str(&format!("{}\n", (next() % 1000000) as f64 / 1000.0));
    }
    let data = dir.join("input.txt");
    std::fs::write(&data, &text).unwrap();
    let src = dir.join("prog.flash");
    std::fs::write(&src, FLOATS.replace("INPUT", data.to_str().unwrap())).unwrap();
    let (b, r) = build_and_run(&src, true, "a64-floats-x86", &[]);
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    let expected = String::from_utf8_lossy(&r.unwrap().stdout).into_owned();
    assert!(expected.lines().count() > 300);
    for target in TARGETS {
        for release in [false, true] {
            assert_eq!(emulate_for(&src, release, target, &format!("a64-floats-{}", release)), expected, "{} (release: {})", target, release);
        }
    }
}

#[test]
fn benchmark_programs_match_x86_64_on_aarch64() {
    if !emulator_available() {
        eprintln!("skipped: python3 with unicorn is not available");
        return;
    }
    for name in ["strings", "sieve", "mandelbrot", "matmul"] {
        let src = repo_root().join("benchmarks").join(format!("{}.flash", name));
        let (b, r) = build_and_run(&src, true, &format!("a64-bench-x86-{}", name), &[]);
        assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
        let expected = String::from_utf8_lossy(&r.unwrap().stdout).into_owned();
        for target in TARGETS {
            assert_eq!(emulate_for(&src, true, target, &format!("a64-bench-{}", name)), expected, "{} {}", name, target);
        }
    }
}

#[test]
fn division_by_constants_matches_x86_64_on_aarch64() {
    if !emulator_available() {
        eprintln!("skipped: python3 with unicorn is not available");
        return;
    }
    let src = repo_root().join("tests/programs/const_division.flash");
    let expected = std::fs::read_to_string(src.with_extension("expected")).unwrap();
    for target in TARGETS {
        for release in [false, true] {
            assert_eq!(emulate_for(&src, release, target, &format!("a64-div-{}", release)), expected, "{} (release: {})", target, release);
        }
    }
}
