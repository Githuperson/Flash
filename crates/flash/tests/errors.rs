//! Every file in tests/errors must be rejected at compile time (exit code 1) with a diagnostic
//! containing each line of the matching `.error` file.

mod common;
use common::*;

#[test]
fn all_error_programs_are_rejected_with_expected_messages() {
    let dir = repo_root().join("tests/errors");
    let mut checked = 0;
    let mut failures = vec![];
    let mut entries: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for p in entries {
        if p.extension().and_then(|e| e.to_str()) != Some("flash") {
            continue;
        }
        let expected = std::fs::read_to_string(p.with_extension("error")).expect("missing .error file");
        let out = flash(&["check", p.to_str().unwrap()]);
        let stderr = text(&out.stderr);
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        if out.status.code() != Some(1) {
            failures.push(format!("{}: expected exit code 1, got {:?}", name, out.status.code()));
        }
        for line in expected.lines().filter(|l| !l.is_empty()) {
            if !stderr.contains(line) {
                failures.push(format!("{}: diagnostic does not contain {:?}\n--- got ---\n{}", name, line, stderr));
            }
        }
        checked += 1;
    }
    assert!(checked >= 40, "only {} error programs found", checked);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
