//! Tests of the native x86-64 back end on hand-built IR (no runtime library involved).

mod common;
use common::*;
use flashc::ir::*;
use std::process::Command;

fn run_elf(bytes: &[u8], name: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("flash-native-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = Command::new(&path).output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
}

fn blank(name: &str, params: Vec<Ty>, ret: Option<Ty>) -> Function {
    Function {
        name: name.into(),
        vregs: params.clone(),
        params,
        ret,
        blocks: vec![Block { insts: vec![], term: Term::Unreachable }],
        stack_slots: vec![],
        exported: false,
        no_inline: false,
    }
}

#[test]
fn hello_through_raw_syscalls() {
    let mut m = Module::default();
    m.data.push(DataItem { name: "msg".into(), bytes: b"hi\n".to_vec(), align: 1, writable: false });
    let mut f = blank("fl_start", vec![Ty::I64], None);
    let (n, fd, p, len, ex, code) = (f.new_vreg(Ty::I64), f.new_vreg(Ty::I64), f.new_vreg(Ty::I64), f.new_vreg(Ty::I64), f.new_vreg(Ty::I64), f.new_vreg(Ty::I64));
    let r = f.new_vreg(Ty::I64);
    let b = &mut f.blocks[0];
    b.insts.push(Inst::IConst { dst: n, val: 1 });
    b.insts.push(Inst::IConst { dst: fd, val: 1 });
    b.insts.push(Inst::DataAddr { dst: p, data: 0 });
    b.insts.push(Inst::IConst { dst: len, val: 3 });
    b.insts.push(Inst::Call { dst: Some(r), callee: Callee::Syscall, args: vec![n, fd, p, len] });
    b.insts.push(Inst::IConst { dst: ex, val: 231 });
    b.insts.push(Inst::IConst { dst: code, val: 7 });
    b.insts.push(Inst::Call { dst: None, callee: Callee::Syscall, args: vec![ex, code] });
    b.term = Term::Unreachable;
    m.funcs.push(f);
    let elf = flashc::native::compile(m, flashc::native::Target::host().unwrap()).unwrap();
    let (status, out) = run_elf(&elf, "hello");
    assert_eq!(status, 7);
    assert_eq!(out, "hi\n");
}

#[test]
fn macos_executables_are_well_formed_mach_o() {
    let dir = scratch_dir("native-macho");
    let exe = dir.join("hello_mac");
    let out = flash(&["build", repo_root().join("examples/hello.flash").to_str().unwrap(), "--target", "x86_64-apple-darwin", "-o", exe.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let bytes = std::fs::read(&exe).unwrap();
    assert_eq!(&bytes[..4], &[0xcf, 0xfa, 0xed, 0xfe], "Mach-O 64-bit magic");
    assert_eq!(&bytes[4..8], &[0x07, 0x00, 0x00, 0x01], "CPU_TYPE_X86_64");
    // When LLVM's tools are installed, let them parse the file and check the entry thread.
    if let Ok(o) = Command::new("llvm-objdump").args(["--macho", "--private-headers"]).arg(&exe).output() {
        let text = String::from_utf8_lossy(&o.stdout);
        assert!(o.status.success(), "llvm-objdump rejected the file: {}", String::from_utf8_lossy(&o.stderr));
        for needle in ["LC_UNIXTHREAD", "__PAGEZERO", "__TEXT", "__DATA", "x86_THREAD_STATE64"] {
            assert!(text.contains(needle), "missing {} in:\n{}", needle, text);
        }
    } else {
        eprintln!("note: llvm-objdump not installed; skipping the Mach-O structure check");
    }
}

#[test]
fn cross_compiling_to_an_unsupported_target_is_a_clear_error() {
    let dir = scratch_dir("native-badtarget");
    let out = flash(&["build", repo_root().join("examples/hello.flash").to_str().unwrap(), "--target", "riscv64-unknown-linux-gnu", "-o", dir.join("x").to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot generate code for 'riscv64"), "{}", String::from_utf8_lossy(&out.stderr));
}
