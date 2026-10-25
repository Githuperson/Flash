//! Instruction selection and emission: IR function → x86-64 machine code.
//!
//! Conventions of Flash's own calling convention (used by all generated code; there is no foreign
//! code in a native Flash program):
//! * integer arguments in rdi, rsi, rdx, rcx, r8, r9; float arguments in xmm0–xmm7; the rest on the
//!   stack (8 bytes each, lowest address first); results in rax / xmm0;
//! * rbx, rbp, r12–r15 and xmm8–xmm13 are preserved by the callee, everything else is clobbered;
//! * integers narrower than 64 bits are always kept zero-extended in registers, slots and arguments.
//!
//! Scratch registers that are never allocated: rax, rcx, rdx, r11, xmm14, xmm15.

use std::collections::HashMap;

use crate::ir::*;

use super::asm::*;
use super::magic;
use super::regalloc::*;

pub const INT_ARG_REGS: [Reg; 6] = [RDI, RSI, RDX, RCX, R8, R9];
pub const FLT_ARG_REGS: [Reg; 8] = [0, 1, 2, 3, 4, 5, 6, 7];
const XT0: Reg = 14;
const XT1: Reg = 15;

#[derive(Clone, Copy, Debug, PartialEq)]
enum ArgLoc {
    Int(Reg),
    Flt(Reg),
    Stack(u32),
}

fn classify(tys: &[Ty]) -> Vec<ArgLoc> {
    let (mut ni, mut nf, mut ns) = (0, 0, 0);
    tys.iter()
        .map(|t| {
            if t.is_float() {
                if nf < FLT_ARG_REGS.len() {
                    nf += 1;
                    ArgLoc::Flt(FLT_ARG_REGS[nf - 1])
                } else {
                    ns += 1;
                    ArgLoc::Stack(ns - 1)
                }
            } else if ni < INT_ARG_REGS.len() {
                ni += 1;
                ArgLoc::Int(INT_ARG_REGS[ni - 1])
            } else {
                ns += 1;
                ArgLoc::Stack(ns - 1)
            }
        })
        .collect()
}

fn sz_of(t: Ty) -> Sz {
    match t {
        Ty::I8 => Sz::B,
        Ty::I16 => Sz::W,
        Ty::I32 => Sz::D,
        _ => Sz::Q,
    }
}

fn bits_of(t: Ty) -> u32 {
    t.bytes() * 8
}

#[derive(Clone, Copy, Debug)]
enum Opd {
    R(Reg),
    M(Mem),
    I(i64),
}

#[derive(Clone, Copy, Debug)]
enum FOpd {
    X(Reg),
    M(Mem),
    C(u64),
}

/// How to test the result of a comparison.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Test {
    Cc(Cc),
    /// Float equality: equal and ordered.
    FEq,
    /// Float inequality: not equal or unordered.
    FNe,
}

/// Cross-function fixups collected during emission.
#[derive(Default)]
pub struct Fixups {
    pub calls: Vec<(usize, u32)>,
    pub data: Vec<(usize, u32)>,
    pub pool: Vec<(usize, usize)>,
}

#[derive(Default)]
pub struct Pool {
    pub entries: Vec<[u8; 16]>,
    index: HashMap<[u8; 16], usize>,
}

impl Pool {
    fn add(&mut self, e: [u8; 16]) -> usize {
        if let Some(i) = self.index.get(&e) {
            return *i;
        }
        self.entries.push(e);
        self.index.insert(e, self.entries.len() - 1);
        self.entries.len() - 1
    }
    fn f64(&mut self, bits: u64) -> usize {
        let mut e = [0u8; 16];
        e[..8].copy_from_slice(&bits.to_le_bytes());
        self.add(e)
    }
    fn mask(&mut self, bits: u64) -> usize {
        let mut e = [0u8; 16];
        e[..8].copy_from_slice(&bits.to_le_bytes());
        e[8..].copy_from_slice(&bits.to_le_bytes());
        self.add(e)
    }
}

pub struct Cg<'m> {
    pub a: Asm,
    pub fix: Fixups,
    pub pool: Pool,
    m: &'m Module,
}

struct FnState<'f> {
    f: &'f Function,
    al: Allocation,
    labels: Vec<Label>,
    out_args: i32,
    slots_base: i32,
    spill_base: i32,
    fsave_base: i32,
    frame: i32,
    /// Offsets of the IR stack slots relative to rsp.
    slot_off: Vec<i32>,
}

impl<'m> Cg<'m> {
    pub fn new(m: &'m Module) -> Cg<'m> {
        Cg { a: Asm::new(), fix: Fixups::default(), pool: Pool::default(), m }
    }

    fn pool_mem(&mut self, idx: usize) -> Mem {
        // The absolute address is patched once the layout is known; the disp32 is the last 4 bytes
        // of the instruction, so the caller records the fixup *after* emitting the instruction.
        let _ = idx;
        Mem::abs(0)
    }

    /// Emit an instruction that references pool entry `idx` via an absolute operand.
    fn with_pool(&mut self, idx: usize, emit: impl FnOnce(&mut Asm, Mem)) {
        let m = self.pool_mem(idx);
        emit(&mut self.a, m);
        let at = self.a.pos() - 4;
        self.fix.pool.push((at, idx));
    }

    /// Emit one function; returns its (start, end) offsets in the code buffer.
    pub fn emit_function(&mut self, f: &Function) -> Result<(usize, usize), String> {
        self.a.align_to(16, 0xCC);
        let start = self.a.pos();
        let al = allocate(f);
        let labels: Vec<Label> = f.blocks.iter().map(|_| self.a.new_label()).collect();

        // Frame layout.
        let mut out_args = 0i32;
        for b in &al.order {
            for inst in &f.blocks[*b].insts {
                if let Inst::Call { callee, args, .. } = inst {
                    let tys = self.call_param_tys(f, callee, args)?;
                    let n = classify(&tys).iter().filter(|l| matches!(l, ArgLoc::Stack(_))).count() as i32;
                    out_args = out_args.max(8 * n);
                }
            }
        }
        let mut off = out_args;
        let slots_base = off;
        let mut slot_off = vec![];
        for s in &f.stack_slots {
            slot_off.push(off);
            off += ((*s as i32) + 7) & !7;
        }
        let spill_base = off;
        off += 8 * al.nslots as i32;
        let fsave_base = off;
        off += 8 * al.saved_flt.len() as i32;
        // rsp is 16-byte aligned at calls: entry rsp = 8 mod 16, pushes add 8 each.
        let pushed = 8 * al.saved_int.len() as i32;
        let mut frame = off;
        while (8 + pushed + frame) % 16 != 0 {
            frame += 8;
        }

        let mut st = FnState { f, al, labels, out_args, slots_base, spill_base, fsave_base, frame, slot_off };
        let _ = (st.out_args, st.slots_base);
        self.prologue(&st);
        for oi in 0..st.al.order.len() {
            let bi = st.al.order[oi];
            let next = st.al.order.get(oi + 1).copied();
            self.a.bind(st.labels[bi]);
            self.block(&mut st, bi, next)?;
        }
        Ok((start, self.a.pos()))
    }

    fn call_param_tys(&self, f: &Function, callee: &Callee, args: &[VReg]) -> Result<Vec<Ty>, String> {
        let _ = callee;
        Ok(args.iter().map(|a| f.vregs[*a as usize]).collect())
    }

    // ------------------------------------------------------------ frame
    fn prologue(&mut self, st: &FnState) {
        for r in &st.al.saved_int {
            self.a.push(*r);
        }
        if st.frame > 0 {
            self.a.alu_ri(Alu::Sub, Sz::Q, RSP, st.frame);
        }
        for (i, x) in st.al.saved_flt.iter().enumerate() {
            self.a.movsd_store(Mem::at(RSP, st.fsave_base + 8 * i as i32), *x);
        }
        // Move incoming arguments to their allocated locations.
        let locs = classify(&st.f.params);
        let mut imoves: Vec<(IDst, ISrc)> = vec![];
        let mut fmoves: Vec<(FDst, FSrc)> = vec![];
        let pushed = 8 * st.al.saved_int.len() as i32;
        for (i, l) in locs.iter().enumerate() {
            let v = i;
            let dst = st.al.loc[v];
            if dst == Loc::Dead {
                continue;
            }
            match l {
                ArgLoc::Int(r) => {
                    imoves.push((self.idst(st, dst), ISrc::R(*r)));
                }
                ArgLoc::Flt(x) => {
                    fmoves.push((self.fdst(st, dst), FSrc::X(*x)));
                }
                ArgLoc::Stack(k) => {
                    let m = Mem::at(RSP, st.frame + pushed + 8 + 8 * *k as i32);
                    if st.f.params[i].is_float() {
                        fmoves.push((self.fdst(st, dst), FSrc::M(m)));
                    } else {
                        imoves.push((self.idst(st, dst), ISrc::M(m)));
                    }
                }
            }
        }
        self.par_move_int(imoves);
        self.par_move_flt(fmoves);
    }

    fn epilogue(&mut self, st: &FnState) {
        for (i, x) in st.al.saved_flt.iter().enumerate() {
            self.a.movsd_load(*x, Mem::at(RSP, st.fsave_base + 8 * i as i32));
        }
        if st.frame > 0 {
            self.a.alu_ri(Alu::Add, Sz::Q, RSP, st.frame);
        }
        for r in st.al.saved_int.iter().rev() {
            self.a.pop(*r);
        }
        self.a.ret();
    }

    fn slot_mem(&self, st: &FnState, s: u32) -> Mem {
        Mem::at(RSP, st.spill_base + 8 * s as i32)
    }

    fn idst(&self, st: &FnState, l: Loc) -> IDst {
        match l {
            Loc::Reg(r) => IDst::R(r),
            Loc::Slot(s) => IDst::M(self.slot_mem(st, s)),
            _ => unreachable!(),
        }
    }
    fn fdst(&self, st: &FnState, l: Loc) -> FDst {
        match l {
            Loc::Reg(r) => FDst::X(r),
            Loc::Slot(s) => FDst::M(self.slot_mem(st, s)),
            _ => unreachable!(),
        }
    }

    // --------------------------------------------------------- operands
    fn opd(&self, st: &FnState, v: VReg) -> Opd {
        match st.al.loc[v as usize] {
            Loc::Reg(r) => Opd::R(r),
            Loc::Slot(s) => Opd::M(self.slot_mem(st, s)),
            Loc::Const => match st.al.consts[v as usize] {
                Some(Const::I(i)) => Opd::I(i),
                Some(Const::F(b)) => Opd::I(b as i64),
                None => unreachable!(),
            },
            Loc::Dead => Opd::I(0),
        }
    }

    fn fopd(&self, st: &FnState, v: VReg) -> FOpd {
        match st.al.loc[v as usize] {
            Loc::Reg(r) => FOpd::X(r),
            Loc::Slot(s) => FOpd::M(self.slot_mem(st, s)),
            Loc::Const => match st.al.consts[v as usize] {
                Some(Const::F(b)) => FOpd::C(b),
                Some(Const::I(i)) => FOpd::C(i as u64),
                None => unreachable!(),
            },
            Loc::Dead => FOpd::C(0),
        }
    }

    /// Put the value of an integer operand into `rd`.
    fn load_opd(&mut self, rd: Reg, o: Opd) {
        match o {
            Opd::R(r) => self.a.mov_rr(Sz::Q, rd, r),
            Opd::M(m) => self.a.load(Sz::Q, rd, m),
            Opd::I(i) => self.a.mov_ri(rd, i),
        }
    }

    /// A register that holds the operand: its own, or `scratch` after loading.
    fn in_reg(&mut self, o: Opd, scratch: Reg) -> Reg {
        match o {
            Opd::R(r) => r,
            _ => {
                self.load_opd(scratch, o);
                scratch
            }
        }
    }

    fn load_fopd(&mut self, xd: Reg, o: FOpd) {
        match o {
            FOpd::X(x) => self.a.movsd_rr(xd, x),
            FOpd::M(m) => self.a.movsd_load(xd, m),
            FOpd::C(0) => self.a.xorpd(xd, xd),
            FOpd::C(b) => {
                let idx = self.pool.f64(b);
                self.with_pool(idx, |a, m| a.movsd_load(xd, m));
            }
        }
    }

    fn f_in_reg(&mut self, o: FOpd, scratch: Reg) -> Reg {
        match o {
            FOpd::X(x) => x,
            _ => {
                self.load_fopd(scratch, o);
                scratch
            }
        }
    }

    /// Register to compute an integer result in.
    fn dst_int(&self, st: &FnState, d: VReg) -> Reg {
        match st.al.loc[d as usize] {
            Loc::Reg(r) => r,
            _ => RAX,
        }
    }
    fn dst_flt(&self, st: &FnState, d: VReg) -> Reg {
        match st.al.loc[d as usize] {
            Loc::Reg(r) => r,
            _ => XT1,
        }
    }

    /// Finish writing an integer result that was computed in `r`.
    fn commit_int(&mut self, st: &FnState, d: VReg, r: Reg) {
        match st.al.loc[d as usize] {
            Loc::Reg(dr) => self.a.mov_rr(Sz::Q, dr, r),
            Loc::Slot(s) => {
                let m = self.slot_mem(st, s);
                self.a.store(Sz::Q, m, r)
            }
            _ => {}
        }
    }
    fn commit_flt(&mut self, st: &FnState, d: VReg, x: Reg) {
        match st.al.loc[d as usize] {
            Loc::Reg(dr) => self.a.movsd_rr(dr, x),
            Loc::Slot(s) => {
                let m = self.slot_mem(st, s);
                self.a.movsd_store(m, x)
            }
            _ => {}
        }
    }

    /// Zero-extend `r` from the width of `t` so that the "narrow values are zero-extended" invariant holds.
    fn norm(&mut self, r: Reg, t: Ty) {
        match t {
            Ty::I8 => self.a.movzx(Sz::B, r, r),
            Ty::I16 => self.a.movzx(Sz::W, r, r),
            Ty::I32 => self.a.movzx(Sz::D, r, r),
            _ => {}
        }
    }

    // ------------------------------------------------------ parallel moves
    fn par_move_int(&mut self, moves: Vec<(IDst, ISrc)>) {
        // Memory destinations first: nothing reads them, and sources are still intact.
        let mut regs: Vec<(Reg, ISrc)> = vec![];
        for (d, s) in moves {
            match d {
                IDst::M(m) => match s {
                    ISrc::R(r) => self.a.store(Sz::Q, m, r),
                    ISrc::M(sm) => {
                        self.a.load(Sz::Q, RAX, sm);
                        self.a.store(Sz::Q, m, RAX);
                    }
                    ISrc::I(i) => {
                        if i32::try_from(i).is_ok() {
                            self.a.store_imm(Sz::Q, m, i as i32);
                        } else {
                            self.a.mov_ri(RAX, i);
                            self.a.store(Sz::Q, m, RAX);
                        }
                    }
                },
                IDst::R(r) => regs.push((r, s)),
            }
        }
        regs.retain(|(d, s)| !matches!(s, ISrc::R(r) if r == d));
        while !regs.is_empty() {
            let pick = (0..regs.len()).find(|&i| {
                let d = regs[i].0;
                !regs.iter().enumerate().any(|(j, (_, s))| j != i && matches!(s, ISrc::R(r) if *r == d))
            });
            match pick {
                Some(i) => {
                    let (d, s) = regs.remove(i);
                    match s {
                        ISrc::R(r) => self.a.mov_rr(Sz::Q, d, r),
                        ISrc::M(m) => self.a.load(Sz::Q, d, m),
                        ISrc::I(v) => self.a.mov_ri(d, v),
                    }
                }
                None => {
                    // Only cycles remain: save one destination in RAX and redirect its readers.
                    let d = regs[0].0;
                    self.a.mov_rr(Sz::Q, RAX, d);
                    for (_, s) in regs.iter_mut() {
                        if matches!(s, ISrc::R(r) if *r == d) {
                            *s = ISrc::R(RAX);
                        }
                    }
                }
            }
        }
    }

    fn par_move_flt(&mut self, moves: Vec<(FDst, FSrc)>) {
        let mut regs: Vec<(Reg, FSrc)> = vec![];
        for (d, s) in moves {
            match d {
                FDst::M(m) => match s {
                    FSrc::X(x) => self.a.movsd_store(m, x),
                    FSrc::M(sm) => {
                        self.a.load(Sz::Q, RAX, sm);
                        self.a.store(Sz::Q, m, RAX);
                    }
                    FSrc::C(b) => {
                        self.a.mov_ri(RAX, b as i64);
                        self.a.store(Sz::Q, m, RAX);
                    }
                },
                FDst::X(x) => regs.push((x, s)),
            }
        }
        regs.retain(|(d, s)| !matches!(s, FSrc::X(r) if r == d));
        while !regs.is_empty() {
            let pick = (0..regs.len()).find(|&i| {
                let d = regs[i].0;
                !regs.iter().enumerate().any(|(j, (_, s))| j != i && matches!(s, FSrc::X(r) if *r == d))
            });
            match pick {
                Some(i) => {
                    let (d, s) = regs.remove(i);
                    match s {
                        FSrc::X(x) => self.a.movsd_rr(d, x),
                        FSrc::M(m) => self.a.movsd_load(d, m),
                        FSrc::C(b) => self.load_fopd(d, FOpd::C(b)),
                    }
                }
                None => {
                    let d = regs[0].0;
                    self.a.movsd_rr(XT0, d);
                    for (_, s) in regs.iter_mut() {
                        if matches!(s, FSrc::X(r) if *r == d) {
                            *s = FSrc::X(XT0);
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------- blocks
    fn block(&mut self, st: &mut FnState, bi: usize, next: Option<usize>) -> Result<(), String> {
        let blk = &st.f.blocks[bi];
        let n = blk.insts.len();
        // Fuse a trailing compare into the conditional branch that consumes it.
        let mut fused: Option<&Inst> = None;
        if st.al.fuse_cmp[bi] {
            fused = blk.insts.last();
        }
        let body = if fused.is_some() { n - 1 } else { n };
        for inst in &blk.insts[..body] {
            self.inst(st, inst)?;
        }
        match &blk.term {
            Term::Jump(t) => {
                if next != Some(*t as usize) {
                    self.a.jmp(st.labels[*t as usize]);
                }
            }
            Term::Br { cond, then_, else_ } => {
                let (t, e) = (*then_ as usize, *else_ as usize);
                let test = match fused {
                    Some(Inst::Cmp { op, a, b, .. }) => self.compare(st, *op, *a, *b),
                    _ => match self.opd(st, *cond) {
                        Opd::I(c) => {
                            let target = if c != 0 { t } else { e };
                            if next != Some(target) {
                                self.a.jmp(st.labels[target]);
                            }
                            return Ok(());
                        }
                        Opd::R(r) => {
                            self.a.test_rr(Sz::B, r, r);
                            Test::Cc(Cc::Ne)
                        }
                        Opd::M(m) => {
                            self.a.alu_mi(Alu::Cmp, Sz::B, m, 0);
                            Test::Cc(Cc::Ne)
                        }
                    },
                };
                self.branch(st, test, t, e, next);
            }
            Term::Ret(v) => {
                if let (Some(v), Some(rt)) = (v, st.f.ret) {
                    if rt.is_float() {
                        let o = self.fopd(st, *v);
                        self.load_fopd(0, o);
                    } else {
                        let o = self.opd(st, *v);
                        self.load_opd(RAX, o);
                    }
                }
                self.epilogue(st);
            }
            Term::Unreachable => self.a.ud2(),
        }
        Ok(())
    }

    fn branch(&mut self, st: &FnState, test: Test, t: usize, e: usize, next: Option<usize>) {
        let (lt, le) = (st.labels[t], st.labels[e]);
        match test {
            Test::Cc(cc) => {
                if next == Some(t) {
                    self.a.jcc(cc.negate(), le);
                } else {
                    self.a.jcc(cc, lt);
                    if next != Some(e) {
                        self.a.jmp(le);
                    }
                }
            }
            Test::FEq => {
                self.a.jcc(Cc::P, le);
                if next == Some(t) {
                    self.a.jcc(Cc::Ne, le);
                } else {
                    self.a.jcc(Cc::E, lt);
                    if next != Some(e) {
                        self.a.jmp(le);
                    }
                }
            }
            Test::FNe => {
                self.a.jcc(Cc::P, lt);
                if next == Some(t) {
                    self.a.jcc(Cc::E, le);
                } else {
                    self.a.jcc(Cc::Ne, lt);
                    if next != Some(e) {
                        self.a.jmp(le);
                    }
                }
            }
        }
    }

    /// Emit a comparison and say how to test its result.
    fn compare(&mut self, st: &FnState, op: CmpOp, a: VReg, b: VReg) -> Test {
        use CmpOp::*;
        if matches!(op, FEq | FNe | FLt | FLe | FGt | FGe) {
            // Operands are arranged so that "above" / "above or equal" are false for NaN.
            let (x, y, cc) = match op {
                FEq => (a, b, Test::FEq),
                FNe => (a, b, Test::FNe),
                FGt => (a, b, Test::Cc(Cc::A)),
                FGe => (a, b, Test::Cc(Cc::Ae)),
                FLt => (b, a, Test::Cc(Cc::A)),
                _ => (b, a, Test::Cc(Cc::Ae)),
            };
            let ox = self.fopd(st, x);
            let oy = self.fopd(st, y);
            let rx = self.f_in_reg(ox, XT0);
            match oy {
                FOpd::X(ry) => self.a.ucomisd(rx, ry),
                FOpd::M(m) => self.a.ucomisd_m(rx, m),
                FOpd::C(_) => {
                    let ry = self.f_in_reg(oy, XT1);
                    self.a.ucomisd(rx, ry)
                }
            }
            return cc;
        }
        let ty = st.f.vregs[a as usize];
        let sz = sz_of(ty);
        let oa = self.opd(st, a);
        let ob = self.opd(st, b);
        let ra = self.in_reg(oa, RAX);
        match ob {
            Opd::R(rb) => self.a.alu_rr(Alu::Cmp, sz, ra, rb),
            Opd::M(m) => self.a.alu_rm(Alu::Cmp, sz, ra, m),
            Opd::I(i) => {
                let fits = match sz {
                    Sz::Q => i32::try_from(i).is_ok(),
                    _ => true,
                };
                if fits {
                    self.a.alu_ri(Alu::Cmp, sz, ra, i as i32);
                } else {
                    self.a.mov_ri_keep_flags(RCX, i);
                    self.a.alu_rr(Alu::Cmp, sz, ra, RCX);
                }
            }
        }
        Test::Cc(match op {
            Eq => Cc::E,
            Ne => Cc::Ne,
            SLt => Cc::L,
            SLe => Cc::Le,
            SGt => Cc::G,
            SGe => Cc::Ge,
            ULt => Cc::B,
            ULe => Cc::Be,
            UGt => Cc::A,
            UGe => Cc::Ae,
            _ => unreachable!(),
        })
    }

    // -------------------------------------------------------- instructions
    fn inst(&mut self, st: &mut FnState, inst: &Inst) -> Result<(), String> {
        match inst {
            Inst::IConst { dst, val } => {
                if st.al.loc[*dst as usize] == Loc::Const || st.al.loc[*dst as usize] == Loc::Dead {
                    return Ok(());
                }
                let v = norm_const(st.f.vregs[*dst as usize], *val);
                match st.al.loc[*dst as usize] {
                    Loc::Reg(r) => self.a.mov_ri(r, v),
                    Loc::Slot(s) => {
                        let m = self.slot_mem(st, s);
                        if i32::try_from(v).is_ok() {
                            self.a.store_imm(Sz::Q, m, v as i32);
                        } else {
                            self.a.mov_ri(RAX, v);
                            self.a.store(Sz::Q, m, RAX);
                        }
                    }
                    _ => {}
                }
            }
            Inst::FConst { dst, val } => {
                if matches!(st.al.loc[*dst as usize], Loc::Const | Loc::Dead) {
                    return Ok(());
                }
                let x = self.dst_flt(st, *dst);
                self.load_fopd(x, FOpd::C(val.to_bits()));
                self.commit_flt(st, *dst, x);
            }
            Inst::Copy { dst, src } => {
                if st.f.vregs[*dst as usize].is_float() {
                    let o = self.fopd(st, *src);
                    let x = self.dst_flt(st, *dst);
                    self.load_fopd(x, o);
                    self.commit_flt(st, *dst, x);
                } else {
                    let o = self.opd(st, *src);
                    match (st.al.loc[*dst as usize], o) {
                        (Loc::Dead, _) | (Loc::Const, _) => {}
                        (Loc::Reg(r), o) => self.load_opd(r, o),
                        (Loc::Slot(s), Opd::R(r)) => {
                            let m = self.slot_mem(st, s);
                            self.a.store(Sz::Q, m, r)
                        }
                        (Loc::Slot(s), Opd::I(i)) if i32::try_from(i).is_ok() => {
                            let m = self.slot_mem(st, s);
                            self.a.store_imm(Sz::Q, m, i as i32)
                        }
                        (Loc::Slot(s), o) => {
                            self.load_opd(RAX, o);
                            let m = self.slot_mem(st, s);
                            self.a.store(Sz::Q, m, RAX)
                        }
                    }
                }
            }
            Inst::Bin { dst, op, a, b } => {
                if st.f.vregs[*dst as usize].is_float() {
                    self.fbin(st, *dst, *op, *a, *b)
                } else {
                    self.ibin(st, *dst, *op, *a, *b)
                }
            }
            Inst::Cmp { dst, op, a, b } => {
                let test = self.compare(st, *op, *a, *b);
                let rd = self.dst_int(st, *dst);
                match test {
                    Test::Cc(cc) => {
                        self.a.setcc(cc, rd);
                        self.a.movzx(Sz::B, rd, rd);
                    }
                    Test::FEq => {
                        self.a.setcc(Cc::E, rd);
                        self.a.setcc(Cc::Np, RCX);
                        self.a.alu_rr(Alu::And, Sz::B, rd, RCX);
                        self.a.movzx(Sz::B, rd, rd);
                    }
                    Test::FNe => {
                        self.a.setcc(Cc::Ne, rd);
                        self.a.setcc(Cc::P, RCX);
                        self.a.alu_rr(Alu::Or, Sz::B, rd, RCX);
                        self.a.movzx(Sz::B, rd, rd);
                    }
                }
                self.commit_int(st, *dst, rd);
            }
            Inst::Un { dst, op, a } => self.un(st, *dst, *op, *a),
            Inst::Cast { dst, op, src } => self.cast(st, *dst, *op, *src),
            Inst::Load { dst, base, off, .. } => {
                let ob = self.opd(st, *base);
                let rb = self.in_reg(ob, RAX);
                let m = Mem::at(rb, *off);
                let t = st.f.vregs[*dst as usize];
                if t.is_float() {
                    let x = self.dst_flt(st, *dst);
                    self.a.movsd_load(x, m);
                    self.commit_flt(st, *dst, x);
                } else {
                    let rd = self.dst_int(st, *dst);
                    self.a.load(sz_of(t), rd, m);
                    self.commit_int(st, *dst, rd);
                }
            }
            Inst::Store { base, off, val, .. } => {
                let ob = self.opd(st, *base);
                let rb = self.in_reg(ob, RAX);
                let m = Mem::at(rb, *off);
                let t = st.f.vregs[*val as usize];
                if t.is_float() {
                    let o = self.fopd(st, *val);
                    let x = self.f_in_reg(o, XT0);
                    self.a.movsd_store(m, x);
                } else {
                    let sz = sz_of(t);
                    match self.opd(st, *val) {
                        Opd::I(i) if i32::try_from(i).is_ok() || sz != Sz::Q => self.a.store_imm(sz, m, i as i32),
                        o => {
                            let r = self.in_reg(o, RCX);
                            self.a.store(sz, m, r);
                        }
                    }
                }
            }
            Inst::LoadIdx { dst, base, index, scale, off, .. } => {
                let ob = self.opd(st, *base);
                let rb = self.in_reg(ob, RAX);
                let oi = self.opd(st, *index);
                let ri = self.in_reg(oi, RCX);
                let m = Mem { base: Some(rb), index: Some((ri, *scale)), disp: *off };
                let t = st.f.vregs[*dst as usize];
                if t.is_float() {
                    let x = self.dst_flt(st, *dst);
                    self.a.movsd_load(x, m);
                    self.commit_flt(st, *dst, x);
                } else {
                    let rd = self.dst_int(st, *dst);
                    self.a.load(sz_of(t), rd, m);
                    self.commit_int(st, *dst, rd);
                }
            }
            Inst::StoreIdx { base, index, scale, off, val, .. } => {
                let ob = self.opd(st, *base);
                let rb = self.in_reg(ob, RAX);
                let oi = self.opd(st, *index);
                let ri = self.in_reg(oi, RDX);
                let m = Mem { base: Some(rb), index: Some((ri, *scale)), disp: *off };
                let t = st.f.vregs[*val as usize];
                if t.is_float() {
                    let o = self.fopd(st, *val);
                    let x = self.f_in_reg(o, XT0);
                    self.a.movsd_store(m, x);
                } else {
                    let sz = sz_of(t);
                    match self.opd(st, *val) {
                        Opd::I(i) if i32::try_from(i).is_ok() || sz != Sz::Q => self.a.store_imm(sz, m, i as i32),
                        o => {
                            let r = self.in_reg(o, RCX);
                            self.a.store(sz, m, r);
                        }
                    }
                }
            }
            Inst::Call { dst, callee, args } => self.call(st, *dst, callee, args)?,
            Inst::DataAddr { dst, data } => {
                let rd = self.dst_int(st, *dst);
                let at = self.a.mov_r32_imm_patch(rd);
                self.fix.data.push((at, *data));
                self.commit_int(st, *dst, rd);
            }
            Inst::StackAddr { dst, slot } => {
                let rd = self.dst_int(st, *dst);
                self.a.lea(rd, Mem::at(RSP, st.slot_off[*slot as usize]));
                self.commit_int(st, *dst, rd);
            }
            Inst::Select { dst, cond, a, b } => self.select(st, *dst, *cond, *a, *b),
        }
        Ok(())
    }

    fn ibin(&mut self, st: &FnState, dst: VReg, op: BinOp, a: VReg, b: VReg) {
        let ty = st.f.vregs[dst as usize];
        let sz = if ty == Ty::I64 { Sz::Q } else { Sz::D };
        match op {
            BinOp::SDiv | BinOp::UDiv | BinOp::SRem | BinOp::URem => return self.divrem(st, dst, op, a, b),
            BinOp::Shl | BinOp::SShr | BinOp::UShr => return self.shift(st, dst, op, a, b),
            _ => {}
        }
        let mut oa = self.opd(st, a);
        let mut ob = self.opd(st, b);
        let commutative = matches!(op, BinOp::Add | BinOp::Mul | BinOp::And | BinOp::Or | BinOp::Xor);
        let mut rd = self.dst_int(st, dst);
        let in_rd = |o: Opd, rd: Reg| matches!(o, Opd::R(r) if r == rd);
        if in_rd(ob, rd) && !in_rd(oa, rd) {
            if commutative {
                std::mem::swap(&mut oa, &mut ob);
            } else {
                rd = RAX;
            }
        }
        // `a + imm` / `a + b` into a different register: use lea.
        if op == BinOp::Add && sz == Sz::Q {
            if let Opd::R(ra) = oa {
                if ra != rd {
                    match ob {
                        Opd::I(i) if i32::try_from(i).is_ok() => {
                            self.a.lea(rd, Mem::at(ra, i as i32));
                            return self.commit_int(st, dst, rd);
                        }
                        Opd::R(rb) if rb != RSP => {
                            self.a.lea_rr(rd, ra, rb);
                            return self.commit_int(st, dst, rd);
                        }
                        _ => {}
                    }
                }
            }
        }
        if let (Opd::I(_), true) = (oa, commutative) {
            if !matches!(ob, Opd::I(_)) {
                std::mem::swap(&mut oa, &mut ob);
            }
        }
        // `imul rd, ra, imm` needs no copy of a.
        if op == BinOp::Mul {
            if let (Opd::R(ra), Opd::I(i)) = (oa, ob) {
                if i32::try_from(i).is_ok() {
                    self.a.imul_rri(sz, rd, ra, i as i32);
                    self.norm(rd, ty);
                    return self.commit_int(st, dst, rd);
                }
            }
        }
        self.load_opd(rd, oa);
        let alu = match op {
            BinOp::Add => Some(Alu::Add),
            BinOp::Sub => Some(Alu::Sub),
            BinOp::And => Some(Alu::And),
            BinOp::Or => Some(Alu::Or),
            BinOp::Xor => Some(Alu::Xor),
            _ => None,
        };
        match (alu, ob) {
            (Some(alu), Opd::R(rb)) => self.a.alu_rr(alu, sz, rd, rb),
            (Some(alu), Opd::M(m)) => self.a.alu_rm(alu, sz, rd, m),
            (Some(alu), Opd::I(i)) if sz == Sz::D || i32::try_from(i).is_ok() => self.a.alu_ri(alu, sz, rd, i as i32),
            (Some(alu), Opd::I(i)) => {
                self.a.mov_ri(RCX, i);
                self.a.alu_rr(alu, sz, rd, RCX);
            }
            (None, Opd::R(rb)) => self.a.imul_rr(sz, rd, rb),
            (None, Opd::M(m)) => self.a.imul_rm(sz, rd, m),
            (None, Opd::I(i)) if i32::try_from(i).is_ok() => self.a.imul_rri(sz, rd, rd, i as i32),
            (None, Opd::I(i)) => {
                self.a.mov_ri(RCX, i);
                self.a.imul_rr(sz, rd, RCX);
            }
        }
        if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) {
            self.norm(rd, ty);
        }
        self.commit_int(st, dst, rd);
    }

    fn divrem(&mut self, st: &FnState, dst: VReg, op: BinOp, a: VReg, b: VReg) {
        let ty = st.f.vregs[dst as usize];
        let signed = matches!(op, BinOp::SDiv | BinOp::SRem);
        let rem = matches!(op, BinOp::SRem | BinOp::URem);
        let oa = self.opd(st, a);
        let ob = self.opd(st, b);
        if let Opd::I(dv) = ob {
            if self.divrem_const(st, dst, signed, rem, oa, dv) {
                return;
            }
        }
        // Divisor into a register that is neither rax nor rdx.
        let rb = match ob {
            Opd::R(r) => r,
            o => {
                self.load_opd(RCX, o);
                RCX
            }
        };
        self.load_opd(RAX, oa);
        if signed && ty != Ty::I64 {
            // Narrow signed values are zero-extended in registers: sign-extend, then divide at 64 bits.
            let from = sz_of(ty);
            self.a.movsx(from, RAX, RAX);
            let d = if rb == RCX {
                RCX
            } else {
                self.a.mov_rr(Sz::Q, RCX, rb);
                RCX
            };
            self.a.movsx(from, d, d);
            self.a.cqo(Sz::Q);
            self.a.div(Sz::Q, d, true);
        } else if signed {
            // MIN / -1 would trap in hardware; Flash defines it as wrapping (x / -1 = -x, x % -1 = 0).
            let normal = self.a.new_label();
            let done = self.a.new_label();
            self.a.alu_ri(Alu::Cmp, Sz::Q, rb, -1);
            self.a.jcc(Cc::Ne, normal);
            if rem {
                self.a.mov_ri(RDX, 0);
            } else {
                self.a.neg(Sz::Q, RAX);
            }
            self.a.jmp(done);
            self.a.bind(normal);
            self.a.cqo(Sz::Q);
            self.a.div(Sz::Q, rb, true);
            self.a.bind(done);
        } else {
            self.a.mov_ri(RDX, 0);
            self.a.div(Sz::Q, rb, false);
        }
        let res = if rem { RDX } else { RAX };
        if signed && ty != Ty::I64 {
            self.norm(res, ty);
        }
        self.commit_int(st, dst, res);
    }

    /// Division or remainder by a constant without a divide instruction. Returns false if the
    /// divisor is zero (the generic path then emits the hardware division).
    fn divrem_const(&mut self, st: &FnState, dst: VReg, signed: bool, rem: bool, oa: Opd, dv: i64) -> bool {
        let ty = st.f.vregs[dst as usize];
        let d_s: i64 = match ty {
            Ty::I8 => dv as u8 as i8 as i64,
            Ty::I16 => dv as u16 as i16 as i64,
            Ty::I32 => dv as u32 as i32 as i64,
            _ => dv,
        };
        let d_u = dv as u64;
        if (signed && d_s == 0) || (!signed && d_u == 0) {
            return false;
        }
        // The dividend, sign-extended (signed) or zero-extended (unsigned) to 64 bits, in a register
        // that is not rax or rdx.
        let ra = if signed && ty != Ty::I64 {
            self.load_opd(RCX, oa);
            self.a.movsx(sz_of(ty), RCX, RCX);
            RCX
        } else {
            match oa {
                Opd::R(r) => r,
                o => {
                    self.load_opd(RCX, o);
                    RCX
                }
            }
        };
        let res;
        if signed {
            let ad = d_s.unsigned_abs();
            if d_s == 1 || d_s == -1 {
                if rem {
                    self.a.mov_ri(RAX, 0);
                } else {
                    self.a.mov_rr(Sz::Q, RAX, ra);
                    if d_s == -1 {
                        self.a.neg(Sz::Q, RAX);
                    }
                }
                res = RAX;
            } else if ad.is_power_of_two() {
                let k = ad.trailing_zeros() as u8;
                // t = a + (a < 0 ? 2^k - 1 : 0)
                self.a.mov_rr(Sz::Q, RAX, ra);
                self.a.shift_i(7, Sz::Q, RAX, 63);
                self.a.shift_i(5, Sz::Q, RAX, 64 - k);
                self.a.alu_rr(Alu::Add, Sz::Q, RAX, ra);
                if rem {
                    self.a.mov_ri(RDX, -(1i64 << k));
                    self.a.alu_rr(Alu::And, Sz::Q, RAX, RDX);
                    self.a.mov_rr(Sz::Q, RDX, ra);
                    self.a.alu_rr(Alu::Sub, Sz::Q, RDX, RAX);
                    res = RDX;
                } else {
                    self.a.shift_i(7, Sz::Q, RAX, k);
                    if d_s < 0 {
                        self.a.neg(Sz::Q, RAX);
                    }
                    res = RAX;
                }
            } else {
                let (m, s) = magic::signed(d_s);
                self.a.mov_ri(RAX, m);
                self.a.mul_wide(ra, true);
                if d_s > 0 && m < 0 {
                    self.a.alu_rr(Alu::Add, Sz::Q, RDX, ra);
                }
                if d_s < 0 && m > 0 {
                    self.a.alu_rr(Alu::Sub, Sz::Q, RDX, ra);
                }
                if s > 0 {
                    self.a.shift_i(7, Sz::Q, RDX, s as u8);
                }
                self.a.mov_rr(Sz::Q, RAX, RDX);
                self.a.shift_i(5, Sz::Q, RAX, 63);
                self.a.alu_rr(Alu::Add, Sz::Q, RDX, RAX);
                if rem {
                    self.imul_const(RDX, d_s);
                    self.a.mov_rr(Sz::Q, RAX, ra);
                    self.a.alu_rr(Alu::Sub, Sz::Q, RAX, RDX);
                    res = RAX;
                } else {
                    res = RDX;
                }
            }
            if ty != Ty::I64 {
                self.norm(res, ty);
            }
        } else if d_u == 1 {
            if rem {
                self.a.mov_ri(RAX, 0);
            } else {
                self.a.mov_rr(Sz::Q, RAX, ra);
            }
            res = RAX;
        } else if d_u.is_power_of_two() {
            self.a.mov_rr(Sz::Q, RAX, ra);
            if rem {
                if d_u - 1 <= i32::MAX as u64 {
                    self.a.alu_ri(Alu::And, Sz::Q, RAX, (d_u - 1) as i32);
                } else {
                    self.a.mov_ri(RDX, (d_u - 1) as i64);
                    self.a.alu_rr(Alu::And, Sz::Q, RAX, RDX);
                }
            } else {
                self.a.shift_i(5, Sz::Q, RAX, d_u.trailing_zeros() as u8);
            }
            res = RAX;
        } else {
            let (m, add, s) = magic::unsigned(d_u);
            self.a.mov_ri(RAX, m as i64);
            self.a.mul_wide(ra, false);
            if !add {
                if s > 0 {
                    self.a.shift_i(5, Sz::Q, RDX, s as u8);
                }
            } else {
                self.a.mov_rr(Sz::Q, RAX, ra);
                self.a.alu_rr(Alu::Sub, Sz::Q, RAX, RDX);
                self.a.shift_i(5, Sz::Q, RAX, 1);
                self.a.alu_rr(Alu::Add, Sz::Q, RAX, RDX);
                if s > 1 {
                    self.a.shift_i(5, Sz::Q, RAX, (s - 1) as u8);
                }
                self.a.mov_rr(Sz::Q, RDX, RAX);
            }
            if rem {
                self.imul_const(RDX, d_u as i64);
                self.a.mov_rr(Sz::Q, RAX, ra);
                self.a.alu_rr(Alu::Sub, Sz::Q, RAX, RDX);
                res = RAX;
            } else {
                res = RDX;
            }
        }
        self.commit_int(st, dst, res);
        true
    }

    /// r *= c (64-bit), using rax as a temporary for constants that do not fit an immediate.
    fn imul_const(&mut self, r: Reg, c: i64) {
        if i32::try_from(c).is_ok() {
            self.a.imul_rri(Sz::Q, r, r, c as i32);
        } else {
            self.a.mov_ri(RAX, c);
            self.a.imul_rr(Sz::Q, r, RAX);
        }
    }

    fn shift(&mut self, st: &FnState, dst: VReg, op: BinOp, a: VReg, b: VReg) {
        let ty = st.f.vregs[dst as usize];
        let bits = bits_of(ty);
        let rd = self.dst_int(st, dst);
        let oa = self.opd(st, a);
        let ob = self.opd(st, b);
        let ext = match op {
            BinOp::Shl => 4,
            BinOp::UShr => 5,
            _ => 7,
        };
        // Operating size: 64-bit for I64 and for signed right shifts (which sign-extend first),
        // 32-bit otherwise.
        let sz = if ty == Ty::I64 || op == BinOp::SShr { Sz::Q } else { Sz::D };
        match ob {
            Opd::I(n) => {
                let n = (n as u64 % bits as u64) as u8;
                self.load_opd(rd, oa);
                if op == BinOp::SShr && ty != Ty::I64 {
                    self.a.movsx(sz_of(ty), rd, rd);
                }
                if n != 0 {
                    self.a.shift_i(ext, sz, rd, n);
                }
            }
            _ => {
                // Count into cl first (it may live in the destination register).
                self.load_opd(RCX, ob);
                if bits < 64 {
                    self.a.alu_ri(Alu::And, Sz::D, RCX, (bits - 1) as i32);
                }
                if matches!(oa, Opd::R(r) if r == rd) {
                    // value already in place
                } else {
                    self.load_opd(rd, oa);
                }
                if op == BinOp::SShr && ty != Ty::I64 {
                    self.a.movsx(sz_of(ty), rd, rd);
                }
                self.a.shift_cl(ext, sz, rd);
            }
        }
        if ty != Ty::I64 {
            self.norm(rd, ty);
        }
        self.commit_int(st, dst, rd);
    }

    fn fbin(&mut self, st: &FnState, dst: VReg, op: BinOp, a: VReg, b: VReg) {
        let opc = match op {
            BinOp::FAdd => 0x58,
            BinOp::FMul => 0x59,
            BinOp::FSub => 0x5C,
            BinOp::FDiv => 0x5E,
            _ => unreachable!("integer op on floats"),
        };
        let commutative = matches!(op, BinOp::FAdd | BinOp::FMul);
        let mut oa = self.fopd(st, a);
        let mut ob = self.fopd(st, b);
        let mut xd = self.dst_flt(st, dst);
        let in_xd = |o: FOpd, x: Reg| matches!(o, FOpd::X(r) if r == x);
        if in_xd(ob, xd) && !in_xd(oa, xd) {
            if commutative {
                std::mem::swap(&mut oa, &mut ob);
            } else {
                xd = XT1;
            }
        }
        self.load_fopd(xd, oa);
        match ob {
            FOpd::X(x) => self.a.sd_op(opc, xd, x),
            FOpd::M(m) => self.a.sd_op_m(opc, xd, m),
            FOpd::C(bits) => {
                let idx = self.pool.f64(bits);
                self.with_pool(idx, |asm, m| asm.sd_op_m(opc, xd, m));
            }
        }
        self.commit_flt(st, dst, xd);
    }

    fn un(&mut self, st: &FnState, dst: VReg, op: UnOp, a: VReg) {
        let ty = st.f.vregs[dst as usize];
        match op {
            UnOp::Neg | UnOp::Not => {
                let rd = self.dst_int(st, dst);
                let o = self.opd(st, a);
                self.load_opd(rd, o);
                let sz = if ty == Ty::I64 { Sz::Q } else { Sz::D };
                if op == UnOp::Neg {
                    self.a.neg(sz, rd);
                } else {
                    self.a.not(sz, rd);
                }
                self.norm(rd, ty);
                self.commit_int(st, dst, rd);
            }
            UnOp::FNeg | UnOp::FAbs => {
                let xd = self.dst_flt(st, dst);
                let o = self.fopd(st, a);
                self.load_fopd(xd, o);
                if op == UnOp::FNeg {
                    let idx = self.pool.mask(0x8000_0000_0000_0000);
                    self.with_pool(idx, |asm, m| asm.xorpd_m(xd, m));
                } else {
                    let idx = self.pool.mask(0x7FFF_FFFF_FFFF_FFFF);
                    self.with_pool(idx, |asm, m| asm.andpd_m(xd, m));
                }
                self.commit_flt(st, dst, xd);
            }
            UnOp::FSqrt => {
                let xd = self.dst_flt(st, dst);
                match self.fopd(st, a) {
                    FOpd::X(x) => self.a.sd_op(0x51, xd, x),
                    FOpd::M(m) => self.a.sd_op_m(0x51, xd, m),
                    o @ FOpd::C(_) => {
                        self.load_fopd(XT0, o);
                        self.a.sd_op(0x51, xd, XT0);
                    }
                }
                self.commit_flt(st, dst, xd);
            }
            UnOp::FFloor | UnOp::FCeil => self.round(st, dst, op == UnOp::FFloor, a),
        }
    }

    /// floor / ceil without SSE4.1: convert through a 64-bit integer when |x| < 2^52.
    fn round(&mut self, st: &FnState, dst: VReg, floor: bool, a: VReg) {
        let oa = self.fopd(st, a);
        let xa = self.f_in_reg(oa, XT0);
        let abs_mask = self.pool.mask(0x7FFF_FFFF_FFFF_FFFF);
        let sign_mask = self.pool.mask(0x8000_0000_0000_0000);
        let big = self.pool.f64(4503599627370496.0f64.to_bits());
        let one = self.pool.f64(1.0f64.to_bits());
        let passthrough = self.a.new_label();
        let done = self.a.new_label();
        // XT1 = |x|; NaN, infinities and |x| >= 2^52 are already integral.
        self.a.movsd_rr(XT1, xa);
        self.with_pool(abs_mask, |asm, m| asm.andpd_m(XT1, m));
        self.with_pool(big, |asm, m| asm.ucomisd_m(XT1, m));
        self.a.jcc(Cc::P, passthrough);
        self.a.jcc(Cc::Ae, passthrough);
        self.a.cvttsd2si(RAX, xa);
        self.a.xorpd(XT1, XT1);
        self.a.cvtsi2sd(XT1, RAX);
        if floor {
            // if y > x then y -= 1
            self.a.ucomisd(XT1, xa);
            let skip = self.a.new_label();
            self.a.jcc(Cc::Be, skip);
            self.with_pool(one, |asm, m| asm.sd_op_m(0x5C, XT1, m));
            self.a.bind(skip);
        } else {
            // if x > y then y += 1
            self.a.ucomisd(xa, XT1);
            let skip = self.a.new_label();
            self.a.jcc(Cc::Be, skip);
            self.with_pool(one, |asm, m| asm.sd_op_m(0x58, XT1, m));
            self.a.bind(skip);
        }
        // Result takes the sign of x (so ceil(-0.5) is -0.0).
        let xs = XT0;
        self.a.movsd_rr(xs, xa);
        self.with_pool(sign_mask, |asm, m| asm.andpd_m(xs, m));
        self.with_pool(abs_mask, |asm, m| asm.andpd_m(XT1, m));
        // orpd XT1, XT0
        self.a.orpd(XT1, xs);
        let xd = self.dst_flt(st, dst);
        self.a.movsd_rr(xd, XT1);
        self.a.jmp(done);
        self.a.bind(passthrough);
        self.a.movsd_rr(xd, xa);
        self.a.bind(done);
        self.commit_flt(st, dst, xd);
    }

    fn select(&mut self, st: &FnState, dst: VReg, cond: VReg, a: VReg, b: VReg) {
        let ty = st.f.vregs[dst as usize];
        let oc = self.opd(st, cond);
        if ty.is_float() {
            let oa = self.fopd(st, a);
            let ob = self.fopd(st, b);
            let xd = XT1;
            let c = self.in_reg(oc, RCX);
            self.load_fopd(xd, ob);
            // movsd does not touch flags, so test after loading `a` is not needed: test first.
            let skip = self.a.new_label();
            self.a.test_rr(Sz::B, c, c);
            self.a.jcc(Cc::E, skip);
            self.load_fopd(xd, oa);
            self.a.bind(skip);
            self.commit_flt(st, dst, xd);
            return;
        }
        let oa = self.opd(st, a);
        let ob = self.opd(st, b);
        let c = self.in_reg(oc, RCX);
        // rax = b; if cond { rax = a }
        self.load_opd(RAX, ob);
        let ra = match oa {
            Opd::R(r) => Some(r),
            Opd::M(_) => None,
            Opd::I(i) => {
                self.a.mov_ri_keep_flags(RDX, i);
                Some(RDX)
            }
        };
        self.a.test_rr(Sz::B, c, c);
        match (ra, oa) {
            (Some(r), _) => self.a.cmov(Cc::Ne, RAX, r),
            (None, Opd::M(m)) => self.a.cmov_m(Cc::Ne, RAX, m),
            _ => unreachable!(),
        }
        self.commit_int(st, dst, RAX);
    }

    fn cast(&mut self, st: &FnState, dst: VReg, op: CastOp, src: VReg) {
        let dt = st.f.vregs[dst as usize];
        let stp = st.f.vregs[src as usize];
        match op {
            CastOp::ZExt => {
                let rd = self.dst_int(st, dst);
                let o = self.opd(st, src);
                self.load_opd(rd, o);
                self.commit_int(st, dst, rd);
            }
            CastOp::SExt => {
                let rd = self.dst_int(st, dst);
                let o = self.opd(st, src);
                let r = self.in_reg(o, rd);
                self.a.movsx(sz_of(stp), rd, r);
                self.norm(rd, dt);
                self.commit_int(st, dst, rd);
            }
            CastOp::Trunc => {
                let rd = self.dst_int(st, dst);
                let o = self.opd(st, src);
                self.load_opd(rd, o);
                self.norm(rd, dt);
                self.commit_int(st, dst, rd);
            }
            CastOp::SToF | CastOp::UToF => {
                let xd = self.dst_flt(st, dst);
                let o = self.opd(st, src);
                let signed = op == CastOp::SToF;
                self.load_opd(RAX, o);
                if signed && stp != Ty::I64 {
                    self.a.movsx(sz_of(stp), RAX, RAX);
                }
                self.a.xorpd(xd, xd);
                if signed || stp != Ty::I64 {
                    self.a.cvtsi2sd(xd, RAX);
                } else {
                    let big = self.a.new_label();
                    let done = self.a.new_label();
                    self.a.test_rr(Sz::Q, RAX, RAX);
                    self.a.jcc(Cc::S, big);
                    self.a.cvtsi2sd(xd, RAX);
                    self.a.jmp(done);
                    self.a.bind(big);
                    self.a.mov_rr(Sz::Q, RCX, RAX);
                    self.a.shift_i(5, Sz::Q, RCX, 1);
                    self.a.alu_ri(Alu::And, Sz::D, RAX, 1);
                    self.a.alu_rr(Alu::Or, Sz::Q, RCX, RAX);
                    self.a.cvtsi2sd(xd, RCX);
                    self.a.sd_op(0x58, xd, xd);
                    self.a.bind(done);
                }
                self.commit_flt(st, dst, xd);
            }
            CastOp::FToS | CastOp::FToU => {
                let o = self.fopd(st, src);
                let x = self.f_in_reg(o, XT0);
                if op == CastOp::FToU && dt == Ty::I64 {
                    self.f_to_u64(x);
                } else {
                    self.f_to_i64_sat(x);
                    if dt != Ty::I64 {
                        // Clamp to the destination range.
                        let bits = bits_of(dt);
                        let (lo, hi): (i64, i64) = if op == CastOp::FToS {
                            (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1)
                        } else {
                            (0, (1i64 << bits) - 1)
                        };
                        self.a.mov_ri(RCX, hi);
                        self.a.alu_rr(Alu::Cmp, Sz::Q, RAX, RCX);
                        self.a.cmov(Cc::G, RAX, RCX);
                        self.a.mov_ri_keep_flags(RCX, lo);
                        self.a.alu_rr(Alu::Cmp, Sz::Q, RAX, RCX);
                        self.a.cmov(Cc::L, RAX, RCX);
                        self.norm(RAX, dt);
                    }
                }
                self.commit_int(st, dst, RAX);
            }
            CastOp::FBitsToI => {
                let o = self.fopd(st, src);
                let x = self.f_in_reg(o, XT0);
                let rd = self.dst_int(st, dst);
                self.a.movq_rx(rd, x);
                self.commit_int(st, dst, rd);
            }
            CastOp::IBitsToF => {
                let o = self.opd(st, src);
                let r = self.in_reg(o, RAX);
                let xd = self.dst_flt(st, dst);
                self.a.movq_xr(xd, r);
                self.commit_flt(st, dst, xd);
            }
        }
    }

    /// rax = saturating (x as i64), NaN → 0.
    fn f_to_i64_sat(&mut self, x: Reg) {
        let ok = self.a.new_label();
        let nan = self.a.new_label();
        let neg = self.a.new_label();
        self.a.cvttsd2si(RAX, x);
        self.a.mov_ri(RCX, i64::MIN);
        self.a.alu_rr(Alu::Cmp, Sz::Q, RAX, RCX);
        self.a.jcc(Cc::Ne, ok);
        // Overflow or NaN: the hardware returned the "integer indefinite" value.
        self.a.ucomisd(x, x);
        self.a.jcc(Cc::P, nan);
        self.a.xorpd(XT1, XT1);
        self.a.ucomisd(x, XT1);
        self.a.jcc(Cc::Be, neg);
        self.a.mov_ri(RAX, i64::MAX);
        self.a.jmp(ok);
        self.a.bind(nan);
        self.a.mov_ri(RAX, 0);
        self.a.jmp(ok);
        self.a.bind(neg);
        self.a.mov_rr(Sz::Q, RAX, RCX);
        self.a.bind(ok);
    }

    /// rax = saturating (x as u64), NaN and negatives → 0.
    fn f_to_u64(&mut self, x: Reg) {
        let two63 = self.pool.f64(9223372036854775808.0f64.to_bits());
        let big = self.a.new_label();
        let done = self.a.new_label();
        let sat = self.a.new_label();
        self.with_pool(two63, |asm, m| asm.ucomisd_m(x, m));
        self.a.jcc(Cc::Ae, big);
        self.a.cvttsd2si(RAX, x);
        self.a.test_rr(Sz::Q, RAX, RAX);
        self.a.jcc(Cc::Ns, done);
        self.a.mov_ri(RAX, 0);
        self.a.jmp(done);
        self.a.bind(big);
        self.a.movsd_rr(XT1, x);
        self.with_pool(two63, |asm, m| asm.sd_op_m(0x5C, XT1, m));
        self.a.cvttsd2si(RAX, XT1);
        self.a.test_rr(Sz::Q, RAX, RAX);
        self.a.jcc(Cc::S, sat);
        self.a.mov_ri(RCX, i64::MIN);
        self.a.alu_rr(Alu::Xor, Sz::Q, RAX, RCX);
        self.a.jmp(done);
        self.a.bind(sat);
        self.a.mov_ri(RAX, -1);
        self.a.bind(done);
    }

    // -------------------------------------------------------------- calls
    fn call(&mut self, st: &FnState, dst: Option<VReg>, callee: &Callee, args: &[VReg]) -> Result<(), String> {
        if let Callee::Syscall = callee {
            // args[0] = number, args[1..] = up to six arguments in rdi, rsi, rdx, r10, r8, r9.
            const SYS_REGS: [Reg; 6] = [RDI, RSI, RDX, R10, R8, R9];
            let mut moves = vec![];
            for (i, a) in args[1..].iter().enumerate() {
                moves.push((IDst::R(SYS_REGS[i]), self.isrc(st, *a)));
            }
            self.par_move_int(moves);
            let n = self.opd(st, args[0]);
            self.load_opd(RAX, n);
            self.a.syscall();
            if let Some(d) = dst {
                self.commit_int(st, d, RAX);
            }
            return Ok(());
        }
        let tys: Vec<Ty> = args.iter().map(|a| st.f.vregs[*a as usize]).collect();
        let locs = classify(&tys);
        let mut imoves = vec![];
        let mut fmoves = vec![];
        for (a, l) in args.iter().zip(&locs) {
            match l {
                ArgLoc::Int(r) => imoves.push((IDst::R(*r), self.isrc(st, *a))),
                ArgLoc::Flt(x) => fmoves.push((FDst::X(*x), self.fsrc(st, *a))),
                ArgLoc::Stack(k) => {
                    let m = Mem::at(RSP, 8 * *k as i32);
                    if st.f.vregs[*a as usize].is_float() {
                        fmoves.push((FDst::M(m), self.fsrc(st, *a)));
                    } else {
                        imoves.push((IDst::M(m), self.isrc(st, *a)));
                    }
                }
            }
        }
        self.par_move_int(imoves);
        self.par_move_flt(fmoves);
        match callee {
            Callee::Func(i) => {
                let at = self.a.call_patch();
                self.fix.calls.push((at, *i));
            }
            Callee::Extern(i) => {
                return Err(format!(
                    "unresolved external function `{}`: the native back end has no C library; `extern fn` needs --backend cranelift or llvm",
                    self.m.externs[*i as usize].name
                ))
            }
            Callee::Indirect(_) => return Err("indirect calls are not supported by the native back end".into()),
            Callee::Syscall => unreachable!(),
        }
        if let Some(d) = dst {
            if st.f.vregs[d as usize].is_float() {
                self.commit_flt(st, d, 0);
            } else {
                self.commit_int(st, d, RAX);
            }
        }
        Ok(())
    }

    fn isrc(&self, st: &FnState, v: VReg) -> ISrc {
        match self.opd(st, v) {
            Opd::R(r) => ISrc::R(r),
            Opd::M(m) => ISrc::M(m),
            Opd::I(i) => ISrc::I(i),
        }
    }
    fn fsrc(&self, st: &FnState, v: VReg) -> FSrc {
        match self.fopd(st, v) {
            FOpd::X(x) => FSrc::X(x),
            FOpd::M(m) => FSrc::M(m),
            FOpd::C(b) => FSrc::C(b),
        }
    }
}

#[derive(Clone, Copy)]
enum IDst {
    R(Reg),
    M(Mem),
}
#[derive(Clone, Copy)]
enum ISrc {
    R(Reg),
    M(Mem),
    I(i64),
}
#[derive(Clone, Copy)]
enum FDst {
    X(Reg),
    M(Mem),
}
#[derive(Clone, Copy)]
enum FSrc {
    X(Reg),
    M(Mem),
    C(u64),
}
