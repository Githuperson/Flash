//! Tests of the native x86-64 back end on hand-built IR (no runtime library involved).

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
    let elf = flashc::native::compile(&m).unwrap();
    let (status, out) = run_elf(&elf, "hello");
    assert_eq!(status, 7);
    assert_eq!(out, "hi\n");
}
