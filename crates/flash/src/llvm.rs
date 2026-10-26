//! Optional LLVM backend: IR → textual LLVM IR → object file via `clang`.
//!
//! This backend exists to give release builds LLVM's optimiser without linking LLVM into the
//! compiler: the Flash IR is printed as LLVM IR (virtual registers become `alloca`s that
//! `mem2reg`/SROA turn into SSA values) and `clang -O3 -c` produces the object file. It needs a
//! `clang` executable at compile time (set `FLASH_CLANG` to choose one) but nothing at run time.
//! Select it with `flash build --release --backend llvm`.

use std::fmt::Write;
use std::process::{Command, Stdio};

use crate::codegen::CodegenOptions;
use crate::ir::*;

fn lty(t: Ty) -> &'static str {
    match t {
        Ty::I8 => "i8",
        Ty::I16 => "i16",
        Ty::I32 => "i32",
        Ty::I64 => "i64",
        Ty::F64 => "double",
    }
}

/// Parameter / return attribute: narrow integers are zero-extended at call boundaries (C ABI).
fn ext(t: Ty) -> &'static str {
    match t {
        Ty::I8 | Ty::I16 => " zeroext",
        _ => "",
    }
}

/// `, !tbaa !N` suffix for an access with alias class `tag` (access tag nodes are `!2t`).
fn tbaa(tag: MemTag) -> String {
    if tag == NO_TAG {
        String::new()
    } else {
        format!(", !tbaa !{}", 2 * tag as u32)
    }
}

fn float_lit(v: f64) -> String {
    format!("0x{:016X}", v.to_bits())
}

struct Emitter<'a> {
    m: &'a Module,
    out: String,
    tmp: u32,
}

impl<'a> Emitter<'a> {
    fn t(&mut self) -> String {
        self.tmp += 1;
        format!("%t{}", self.tmp)
    }

    fn fname(&self, i: u32) -> String {
        format!("@\"{}\"", self.m.funcs[i as usize].name)
    }

    fn load_v(&mut self, f: &Function, v: VReg) -> String {
        let t = self.t();
        let ty = lty(f.vregs[v as usize]);
        let _ = writeln!(self.out, "  {} = load {}, ptr %v{}, align {}", t, ty, v, f.vregs[v as usize].bytes());
        t
    }

    fn store_v(&mut self, f: &Function, v: VReg, val: &str) {
        let _ = writeln!(self.out, "  store {} {}, ptr %v{}, align {}", lty(f.vregs[v as usize]), val, v, f.vregs[v as usize].bytes());
    }

    fn addr(&mut self, base: String, off: i32) -> String {
        let p = self.t();
        let _ = writeln!(self.out, "  {} = inttoptr i64 {} to ptr", p, base);
        if off == 0 {
            return p;
        }
        let q = self.t();
        let _ = writeln!(self.out, "  {} = getelementptr i8, ptr {}, i64 {}", q, p, off);
        q
    }

    fn function(&mut self, f: &Function) {
        let linkage = if f.exported { "" } else { "internal " };
        let ret = f.ret.map(lty).unwrap_or("void");
        let ret_attr = f.ret.map(ext).unwrap_or("");
        let params: Vec<String> = f.params.iter().enumerate().map(|(i, t)| format!("{}{} %p{}", lty(*t), ext(*t), i)).collect();
        let _ = writeln!(self.out, "define {}{} {} @\"{}\"({}) {{", linkage, ret_attr.trim_start(), ret, f.name, params.join(", "));
        let _ = writeln!(self.out, "entry:");
        for (i, t) in f.vregs.iter().enumerate() {
            let _ = writeln!(self.out, "  %v{} = alloca {}, align {}", i, lty(*t), t.bytes());
        }
        for (i, sz) in f.stack_slots.iter().enumerate() {
            let _ = writeln!(self.out, "  %slot{} = alloca [{} x i8], align 8", i, sz);
        }
        for (i, t) in f.params.iter().enumerate() {
            let _ = writeln!(self.out, "  store {} %p{}, ptr %v{}, align {}", lty(*t), i, i, t.bytes());
        }
        let _ = writeln!(self.out, "  br label %b0");
        for (bi, b) in f.blocks.iter().enumerate() {
            let _ = writeln!(self.out, "b{}:", bi);
            for inst in &b.insts {
                self.inst(f, inst);
            }
            match &b.term {
                Term::Jump(t) => {
                    let _ = writeln!(self.out, "  br label %b{}", t);
                }
                Term::Br { cond, then_, else_ } => {
                    let c = self.load_v(f, *cond);
                    let c1 = self.t();
                    let _ = writeln!(self.out, "  {} = icmp ne i8 {}, 0", c1, c);
                    let _ = writeln!(self.out, "  br i1 {}, label %b{}, label %b{}", c1, then_, else_);
                }
                Term::Ret(Some(v)) if f.ret.is_some() => {
                    let x = self.load_v(f, *v);
                    let _ = writeln!(self.out, "  ret {} {}", ret, x);
                }
                Term::Ret(_) => {
                    if f.ret.is_some() {
                        let _ = writeln!(self.out, "  unreachable");
                    } else {
                        let _ = writeln!(self.out, "  ret void");
                    }
                }
                Term::Unreachable => {
                    let _ = writeln!(self.out, "  unreachable");
                }
            }
        }
        let _ = writeln!(self.out, "}}\n");
    }

    fn inst(&mut self, f: &Function, inst: &Inst) {
        match inst {
            Inst::IConst { dst, val } => {
                let ty = f.vregs[*dst as usize];
                let v = match ty {
                    Ty::I8 => *val as i8 as i64,
                    Ty::I16 => *val as i16 as i64,
                    Ty::I32 => *val as i32 as i64,
                    _ => *val,
                };
                self.store_v(f, *dst, &v.to_string());
            }
            Inst::FConst { dst, val } => self.store_v(f, *dst, &float_lit(*val)),
            Inst::Copy { dst, src } => {
                let x = self.load_v(f, *src);
                self.store_v(f, *dst, &x);
            }
            Inst::Bin { dst, op, a, b } => {
                let ty = f.vregs[*a as usize];
                let x = self.load_v(f, *a);
                let mut y = self.load_v(f, *b);
                let r = self.t();
                let name = match op {
                    BinOp::Add => "add",
                    BinOp::Sub => "sub",
                    BinOp::Mul => "mul",
                    BinOp::SDiv => "sdiv",
                    BinOp::UDiv => "udiv",
                    BinOp::SRem => "srem",
                    BinOp::URem => "urem",
                    BinOp::And => "and",
                    BinOp::Or => "or",
                    BinOp::Xor => "xor",
                    BinOp::Shl => "shl",
                    BinOp::SShr => "ashr",
                    BinOp::UShr => "lshr",
                    BinOp::FAdd => "fadd",
                    BinOp::FSub => "fsub",
                    BinOp::FMul => "fmul",
                    BinOp::FDiv => "fdiv",
                };
                if matches!(op, BinOp::Shl | BinOp::SShr | BinOp::UShr) {
                    // Flash defines shift amounts modulo the bit width (LLVM would make it poison).
                    let m = self.t();
                    let _ = writeln!(self.out, "  {} = and {} {}, {}", m, lty(ty), y, ty.bytes() * 8 - 1);
                    y = m;
                }
                let _ = writeln!(self.out, "  {} = {} {} {}, {}", r, name, lty(ty), x, y);
                self.store_v(f, *dst, &r);
            }
            Inst::Cmp { dst, op, a, b } => {
                let ty = f.vregs[*a as usize];
                let x = self.load_v(f, *a);
                let y = self.load_v(f, *b);
                let r = self.t();
                let (instr, cc) = match op {
                    CmpOp::Eq => ("icmp", "eq"),
                    CmpOp::Ne => ("icmp", "ne"),
                    CmpOp::SLt => ("icmp", "slt"),
                    CmpOp::SLe => ("icmp", "sle"),
                    CmpOp::SGt => ("icmp", "sgt"),
                    CmpOp::SGe => ("icmp", "sge"),
                    CmpOp::ULt => ("icmp", "ult"),
                    CmpOp::ULe => ("icmp", "ule"),
                    CmpOp::UGt => ("icmp", "ugt"),
                    CmpOp::UGe => ("icmp", "uge"),
                    CmpOp::FEq => ("fcmp", "oeq"),
                    CmpOp::FNe => ("fcmp", "une"),
                    CmpOp::FLt => ("fcmp", "olt"),
                    CmpOp::FLe => ("fcmp", "ole"),
                    CmpOp::FGt => ("fcmp", "ogt"),
                    CmpOp::FGe => ("fcmp", "oge"),
                };
                let _ = writeln!(self.out, "  {} = {} {} {} {}, {}", r, instr, cc, lty(ty), x, y);
                let z = self.t();
                let _ = writeln!(self.out, "  {} = zext i1 {} to i8", z, r);
                self.store_v(f, *dst, &z);
            }
            Inst::Un { dst, op, a } => {
                let ty = f.vregs[*a as usize];
                let x = self.load_v(f, *a);
                let r = self.t();
                match op {
                    UnOp::Neg => {
                        let _ = writeln!(self.out, "  {} = sub {} 0, {}", r, lty(ty), x);
                    }
                    UnOp::Not => {
                        let _ = writeln!(self.out, "  {} = xor {} {}, -1", r, lty(ty), x);
                    }
                    UnOp::FNeg => {
                        let _ = writeln!(self.out, "  {} = fneg double {}", r, x);
                    }
                    UnOp::FSqrt => {
                        let _ = writeln!(self.out, "  {} = call double @llvm.sqrt.f64(double {})", r, x);
                    }
                    UnOp::FFloor => {
                        let _ = writeln!(self.out, "  {} = call double @llvm.floor.f64(double {})", r, x);
                    }
                    UnOp::FCeil => {
                        let _ = writeln!(self.out, "  {} = call double @llvm.ceil.f64(double {})", r, x);
                    }
                    UnOp::FAbs => {
                        let _ = writeln!(self.out, "  {} = call double @llvm.fabs.f64(double {})", r, x);
                    }
                }
                self.store_v(f, *dst, &r);
            }
            Inst::Cast { dst, op, src } => {
                let from = f.vregs[*src as usize];
                let to = f.vregs[*dst as usize];
                let x = self.load_v(f, *src);
                let r = self.t();
                match op {
                    CastOp::ZExt => {
                        let _ = writeln!(self.out, "  {} = zext {} {} to {}", r, lty(from), x, lty(to));
                    }
                    CastOp::SExt => {
                        let _ = writeln!(self.out, "  {} = sext {} {} to {}", r, lty(from), x, lty(to));
                    }
                    CastOp::Trunc => {
                        let _ = writeln!(self.out, "  {} = trunc {} {} to {}", r, lty(from), x, lty(to));
                    }
                    CastOp::SToF => {
                        let _ = writeln!(self.out, "  {} = sitofp {} {} to double", r, lty(from), x);
                    }
                    CastOp::UToF => {
                        let _ = writeln!(self.out, "  {} = uitofp {} {} to double", r, lty(from), x);
                    }
                    CastOp::FToS => {
                        let _ = writeln!(self.out, "  {} = call {} @llvm.fptosi.sat.{}.f64(double {})", r, lty(to), lty(to), x);
                    }
                    CastOp::FToU => {
                        let _ = writeln!(self.out, "  {} = call {} @llvm.fptoui.sat.{}.f64(double {})", r, lty(to), lty(to), x);
                    }
                    CastOp::FBitsToI | CastOp::IBitsToF => {
                        let _ = writeln!(self.out, "  {} = bitcast {} {} to {}", r, lty(from), x, lty(to));
                    }
                }
                self.store_v(f, *dst, &r);
            }
            Inst::Load { dst, base, off, tag } => {
                let ty = f.vregs[*dst as usize];
                let b = self.load_v(f, *base);
                let p = self.addr(b, *off);
                let r = self.t();
                let _ = writeln!(self.out, "  {} = load {}, ptr {}, align {}{}", r, lty(ty), p, ty.bytes(), tbaa(*tag));
                self.store_v(f, *dst, &r);
            }
            Inst::Store { base, off, val, tag } => {
                let ty = f.vregs[*val as usize];
                let b = self.load_v(f, *base);
                let v = self.load_v(f, *val);
                let p = self.addr(b, *off);
                let _ = writeln!(self.out, "  store {} {}, ptr {}, align {}{}", lty(ty), v, p, ty.bytes(), tbaa(*tag));
            }
            Inst::Call { dst, callee, args } => {
                let (name, params, ret): (String, Vec<Ty>, Option<Ty>) = match callee {
                    Callee::Func(i) => {
                        let g = &self.m.funcs[*i as usize];
                        (self.fname(*i), g.params.clone(), g.ret)
                    }
                    Callee::Extern(i) => {
                        let e = &self.m.externs[*i as usize];
                        (format!("@{}", e.name), e.params.clone(), e.ret)
                    }
                    Callee::Indirect(_) => unreachable!("indirect calls are not generated"),
                    Callee::Syscall => unreachable!("system calls exist only in the native back end"),
                };
                let mut argv = vec![];
                for (a, pt) in args.iter().zip(params.iter()) {
                    let x = self.load_v(f, *a);
                    argv.push(format!("{}{} {}", lty(*pt), ext(*pt), x));
                }
                let rty = ret.map(lty).unwrap_or("void");
                let rattr = ret.map(ext).unwrap_or("");
                match dst {
                    Some(d) => {
                        let r = self.t();
                        let _ = writeln!(self.out, "  {} = call{} {} {}({})", r, rattr, rty, name, argv.join(", "));
                        self.store_v(f, *d, &r);
                    }
                    None => {
                        let _ = writeln!(self.out, "  call{} {} {}({})", rattr, rty, name, argv.join(", "));
                    }
                }
            }
            Inst::LoadIdx { .. } | Inst::StoreIdx { .. } => unreachable!("indexed memory instructions exist only in the native back end"),
            Inst::ThreadPtr { .. } => unreachable!("the thread pointer exists only in the native back end"),
            Inst::DataAddr { dst, data } => {
                let r = self.t();
                let _ = writeln!(self.out, "  {} = ptrtoint ptr @data{} to i64", r, data);
                self.store_v(f, *dst, &r);
            }
            Inst::StackAddr { dst, slot } => {
                let r = self.t();
                let _ = writeln!(self.out, "  {} = ptrtoint ptr %slot{} to i64", r, slot);
                self.store_v(f, *dst, &r);
            }
            Inst::Select { dst, cond, a, b } => {
                let ty = f.vregs[*a as usize];
                let c = self.load_v(f, *cond);
                let x = self.load_v(f, *a);
                let y = self.load_v(f, *b);
                let c1 = self.t();
                let _ = writeln!(self.out, "  {} = icmp ne i8 {}, 0", c1, c);
                let r = self.t();
                let _ = writeln!(self.out, "  {} = select i1 {}, {} {}, {} {}", r, c1, lty(ty), x, lty(ty), y);
                self.store_v(f, *dst, &r);
            }
        }
    }
}

pub fn emit_ir(m: &Module) -> String {
    let mut e = Emitter { m, out: String::new(), tmp: 0 };
    let _ = writeln!(e.out, "; Generated by the Flash LLVM backend");
    for (i, d) in m.data.iter().enumerate() {
        let mut bytes = String::new();
        for b in &d.bytes {
            let _ = write!(bytes, "\\{:02X}", b);
        }
        let _ = writeln!(e.out, "@data{} = private unnamed_addr constant [{} x i8] c\"{}\", align 8", i, d.bytes.len(), bytes);
    }
    for x in &m.externs {
        let params: Vec<String> = x.params.iter().map(|t| format!("{}{}", lty(*t), ext(*t))).collect();
        let ret = x.ret.map(lty).unwrap_or("void");
        let rattr = x.ret.map(ext).unwrap_or("");
        let _ = writeln!(e.out, "declare{} {} @{}({})", rattr, ret, x.name, params.join(", "));
    }
    let _ = writeln!(e.out, "declare double @llvm.sqrt.f64(double)\ndeclare double @llvm.floor.f64(double)\ndeclare double @llvm.ceil.f64(double)\ndeclare double @llvm.fabs.f64(double)");
    let _ = writeln!(e.out, "declare i64 @llvm.fptosi.sat.i64.f64(double)\ndeclare i32 @llvm.fptosi.sat.i32.f64(double)\ndeclare i64 @llvm.fptoui.sat.i64.f64(double)\n");
    for f in &m.funcs {
        e.function(f);
    }
    // Type-based alias analysis: accesses with different (unrelated) classes never alias, e.g. a
    // store to a Float list element cannot change a list's length or data pointer, so LLVM may
    // keep those in registers across the store.
    let _ = writeln!(e.out, "!0 = !{{!\"flash tbaa root\"}}");
    for (i, name) in m.tags.iter().enumerate().skip(1) {
        let t = i as u32;
        let _ = writeln!(e.out, "!{} = !{{!\"{}\", !0, i64 0}}", 2 * t - 1, name);
        let _ = writeln!(e.out, "!{} = !{{!{}, !{}, i64 0}}", 2 * t, 2 * t - 1, 2 * t - 1);
    }
    e.out
}

pub fn find_clang() -> Option<String> {
    if let Ok(c) = std::env::var("FLASH_CLANG") {
        return Some(c);
    }
    ["clang", "clang-18", "clang-17", "clang-19", "clang-16", "clang-15"]
        .iter()
        .find(|c| Command::new(c).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success()))
        .map(|s| s.to_string())
}

pub fn compile(m: &Module, opts: &CodegenOptions) -> Result<Vec<u8>, String> {
    let clang = find_clang()
        .ok_or("the LLVM backend needs `clang` on the PATH (or set FLASH_CLANG); use the default Cranelift backend otherwise")?;
    let ll = emit_ir(m);
    let dir = std::env::temp_dir().join(format!("flash-llvm-{}-{}", std::process::id(), m.funcs.len()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let ll_path = dir.join("program.ll");
    let obj_path = dir.join("program.o");
    std::fs::write(&ll_path, &ll).map_err(|e| e.to_string())?;
    let mut cmd = Command::new(&clang);
    cmd.arg(if opts.release { "-O3" } else { "-O0" }).arg("-fPIC").arg("-c").args(["-x", "ir"]).arg(&ll_path).arg("-o").arg(&obj_path);
    cmd.arg("-Wno-override-module");
    if let Some(t) = &opts.target {
        cmd.arg(format!("--target={}", t));
    }
    let out = cmd.output().map_err(|e| format!("cannot run {}: {}", clang, e))?;
    if !out.status.success() {
        let _ = std::fs::write(dir.join("program.ll"), &ll);
        return Err(format!(
            "{} failed on the generated LLVM IR (kept in {}):\n{}",
            clang,
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let bytes = std::fs::read(&obj_path).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(bytes)
}
