//! Instruction selection and emission for AArch64: IR function → machine code.
//!
//! Flash's own calling convention (like AAPCS64, but nothing outside the program relies on it):
//! integer arguments in x0–x7, floats in d0–d7, the rest on the stack (8 bytes each, lowest address
//! first); results in x0 / d0; x19–x28 and d8–d15 are preserved by the callee, x30 (the link
//! register) is saved by every function that makes calls. Integers narrower than 64 bits are always
//! kept zero-extended in registers, spill slots and arguments.
//!
//! Registers that are never allocated: x0–x7 (arguments, results and scratch), x8, x16, x17
//! (scratch), x18 (reserved by macOS), x29 (frame pointer, unused), x30, sp; d0–d7 (arguments and
//! results), d30, d31 (scratch). Because the argument registers are outside the allocation pool,
//! setting up a call is a sequence of independent moves.

use crate::ir::*;

use super::a64::*;
use super::regalloc::*;
use super::Target;

const T0: Reg = 16; // source scratch
const T1: Reg = 17; // source scratch
const T2: Reg = 8; // extra scratch
const D0: Reg = 30; // float scratch
const D1: Reg = 31; // float scratch

const INT_ARGS: usize = 8;
const FLT_ARGS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
enum ArgLoc {
    Int(Reg),
    Flt(Reg),
    Stack(u32),
}

fn classify(tys: &[Ty]) -> Vec<ArgLoc> {
    let (mut ni, mut nf, mut ns) = (0u8, 0u8, 0u32);
    tys.iter()
        .map(|t| {
            if t.is_float() {
                if (nf as usize) < FLT_ARGS {
                    nf += 1;
                    ArgLoc::Flt(nf - 1)
                } else {
                    ns += 1;
                    ArgLoc::Stack(ns - 1)
                }
            } else if (ni as usize) < INT_ARGS {
                ni += 1;
                ArgLoc::Int(ni - 1)
            } else {
                ns += 1;
                ArgLoc::Stack(ns - 1)
            }
        })
        .collect()
}

fn bits_of(t: Ty) -> u32 {
    t.bytes() * 8
}

fn width_of(t: Ty) -> Width {
    match t {
        Ty::I8 => Width::B,
        Ty::I16 => Width::H,
        Ty::I32 => Width::W,
        _ => Width::X,
    }
}

#[derive(Clone, Copy, Debug)]
enum Src {
    R(Reg),
    /// sp-relative spill slot
    M(i32),
    I(i64),
}

#[derive(Default)]
pub struct Fixups {
    /// (offset of the `bl`, callee function index)
    pub calls: Vec<(usize, u32)>,
    /// (offset of the `adrp`; the `add` follows it, data item index)
    pub data: Vec<(usize, u32)>,
}

struct FnState<'f> {
    f: &'f Function,
    al: Allocation,
    labels: Vec<Label>,
    slot_off: Vec<i32>,
    spill_base: i32,
    /// (register, sp offset, is_float) for everything the prologue saves
    saves: Vec<(Reg, i32, bool)>,
    frame: u32,
}

pub struct Cg<'m> {
    target: Target,
    pub a: Asm,
    pub fix: Fixups,
    m: &'m Module,
}

impl<'m> Cg<'m> {
    pub fn new(m: &'m Module, target: Target) -> Cg<'m> {
        Cg { target, a: Asm::new(), fix: Fixups::default(), m }
    }

    /// Emit one function; returns its (start, end) offsets in the code buffer.
    pub fn emit_function(&mut self, f: &Function) -> Result<(usize, usize), String> {
        self.a.align_to(16);
        let start = self.a.pos();
        let al = allocate(f, &AARCH64);
        let labels: Vec<Label> = f.blocks.iter().map(|_| self.a.new_label()).collect();

        let mut out_args = 0i32;
        let mut has_calls = false;
        for b in &al.order {
            for inst in &f.blocks[*b].insts {
                if let Inst::Call { callee, args, .. } = inst {
                    if !matches!(callee, Callee::Syscall) {
                        has_calls = true;
                        let tys: Vec<Ty> = args.iter().map(|a| f.vregs[*a as usize]).collect();
                        let n = classify(&tys).iter().filter(|l| matches!(l, ArgLoc::Stack(_))).count() as i32;
                        out_args = out_args.max(8 * n);
                    }
                }
            }
        }
        let mut off = out_args;
        let mut slot_off = vec![];
        for s in &f.stack_slots {
            slot_off.push(off);
            off += ((*s as i32) + 7) & !7;
        }
        let spill_base = off;
        off += 8 * al.nslots as i32;
        let mut saves = vec![];
        for r in &al.saved_int {
            saves.push((*r, off, false));
            off += 8;
        }
        for r in &al.saved_flt {
            saves.push((*r, off, true));
            off += 8;
        }
        if has_calls {
            saves.push((LR, off, false));
            off += 8;
        }
        let frame = ((off + 15) & !15) as u32;
        let st = FnState { f, al, labels, slot_off, spill_base, saves, frame };

        self.prologue(&st);
        for oi in 0..st.al.order.len() {
            let bi = st.al.order[oi];
            let next = st.al.order.get(oi + 1).copied();
            self.a.bind(st.labels[bi]);
            self.block(&st, bi, next)?;
        }
        Ok((start, self.a.pos()))
    }

    // ------------------------------------------------------------ frame
    fn adjust_sp(&mut self, amount: u32, down: bool) {
        // amount is a multiple of 16; large frames use two immediates (12 bits, optionally shifted)
        let hi = amount & !0xFFF;
        let lo = amount & 0xFFF;
        let op = |a: &mut Asm, v: u32| if down { a.sub_imm(SP, SP, v) } else { a.add_imm(SP, SP, v) };
        if hi > 0 {
            op(&mut self.a, hi);
        }
        if lo > 0 || hi == 0 {
            if amount > 0 {
                op(&mut self.a, lo);
            }
        }
    }

    /// Register holding the address `base + off`; `base` may be sp. Uses T1 when the offset does
    /// not fit the instruction.
    fn addr(&mut self, base: Reg, off: i32) -> Reg {
        if off == 0 && base != SP {
            return base;
        }
        if (0..4096).contains(&off) {
            self.a.add_imm(T1, base, off as u32);
        } else if (-4095..0).contains(&off) {
            self.a.sub_imm(T1, base, (-off) as u32);
        } else {
            self.a.mov_imm(T1, off as i64);
            if base == SP {
                self.a.add_sp_reg(T1, T1);
            } else {
                self.a.add(T1, base, T1);
            }
        }
        T1
    }

    fn ld(&mut self, w: Width, rt: Reg, base: Reg, off: i32) {
        if !self.a.ldr(w, rt, base, off) {
            let r = self.addr(base, off);
            let ok = self.a.ldr(w, rt, r, 0);
            debug_assert!(ok);
        }
    }
    fn st(&mut self, w: Width, rt: Reg, base: Reg, off: i32) {
        if !self.a.str(w, rt, base, off) {
            let r = self.addr(base, off);
            let ok = self.a.str(w, rt, r, 0);
            debug_assert!(ok);
        }
    }
    fn ld_d(&mut self, dt: Reg, base: Reg, off: i32) {
        if !self.a.ldr_d(dt, base, off) {
            let r = self.addr(base, off);
            let ok = self.a.ldr_d(dt, r, 0);
            debug_assert!(ok);
        }
    }
    fn st_d(&mut self, dt: Reg, base: Reg, off: i32) {
        if !self.a.str_d(dt, base, off) {
            let r = self.addr(base, off);
            let ok = self.a.str_d(dt, r, 0);
            debug_assert!(ok);
        }
    }

    fn prologue(&mut self, st: &FnState) {
        self.adjust_sp(st.frame, true);
        for &(r, off, flt) in &st.saves {
            if flt {
                self.st_d(r, SP, off);
            } else {
                self.st(Width::X, r, SP, off);
            }
        }
        // Incoming arguments → their allocated locations.
        let locs = classify(&st.f.params);
        for (i, l) in locs.iter().enumerate() {
            let dst = st.al.loc[i];
            if dst == Loc::Dead {
                continue;
            }
            let flt = st.f.params[i].is_float();
            match (l, flt) {
                (ArgLoc::Int(r), _) => self.store_loc(st, dst, *r),
                (ArgLoc::Flt(r), _) => self.store_floc(st, dst, *r),
                (ArgLoc::Stack(k), false) => {
                    self.ld(Width::X, T0, SP, st.frame as i32 + 8 * *k as i32);
                    self.store_loc(st, dst, T0);
                }
                (ArgLoc::Stack(k), true) => {
                    self.ld_d(D0, SP, st.frame as i32 + 8 * *k as i32);
                    self.store_floc(st, dst, D0);
                }
            }
        }
    }

    fn epilogue(&mut self, st: &FnState) {
        for &(r, off, flt) in &st.saves {
            if flt {
                self.ld_d(r, SP, off);
            } else {
                self.ld(Width::X, r, SP, off);
            }
        }
        self.adjust_sp(st.frame, false);
        self.a.ret();
    }

    // --------------------------------------------------------- operands
    fn slot_off(&self, st: &FnState, s: u32) -> i32 {
        st.spill_base + 8 * s as i32
    }

    fn src(&self, st: &FnState, v: VReg) -> Src {
        match st.al.loc[v as usize] {
            Loc::Reg(r) => Src::R(r),
            Loc::Slot(s) => Src::M(self.slot_off(st, s)),
            Loc::Const => match st.al.consts[v as usize] {
                Some(Const::I(i)) => Src::I(i),
                Some(Const::F(b)) => Src::I(b as i64),
                None => unreachable!(),
            },
            Loc::Dead => Src::I(0),
        }
    }

    /// A register holding the integer operand: its own, or `scratch` after loading.
    fn in_reg(&mut self, s: Src, scratch: Reg) -> Reg {
        match s {
            Src::R(r) => r,
            Src::M(off) => {
                self.ld(Width::X, scratch, SP, off);
                scratch
            }
            // never XZR: register 31 means SP in address and add/sub-immediate operands
            Src::I(i) => {
                self.a.mov_imm(scratch, i);
                scratch
            }
        }
    }

    /// A floating-point register holding the operand.
    fn in_freg(&mut self, st: &FnState, v: VReg, scratch: Reg) -> Reg {
        match st.al.loc[v as usize] {
            Loc::Reg(r) => r,
            Loc::Slot(s) => {
                let off = self.slot_off(st, s);
                self.ld_d(scratch, SP, off);
                scratch
            }
            _ => {
                let bits = match st.al.consts[v as usize] {
                    Some(Const::F(b)) => b,
                    Some(Const::I(i)) => i as u64,
                    None => 0,
                };
                self.load_fbits(scratch, bits);
                scratch
            }
        }
    }

    fn load_fbits(&mut self, d: Reg, bits: u64) {
        if bits == 0 {
            self.a.fmov_from_x(d, XZR);
        } else {
            self.a.mov_imm(T2, bits as i64);
            self.a.fmov_from_x(d, T2);
        }
    }

    fn def_reg(&self, st: &FnState, d: VReg) -> Reg {
        match st.al.loc[d as usize] {
            Loc::Reg(r) => r,
            _ => 0,
        }
    }
    fn def_freg(&self, st: &FnState, d: VReg) -> Reg {
        match st.al.loc[d as usize] {
            Loc::Reg(r) => r,
            _ => D0,
        }
    }

    fn store_loc(&mut self, st: &FnState, l: Loc, r: Reg) {
        match l {
            Loc::Reg(dr) => {
                if dr != r {
                    self.a.mov(dr, r)
                }
            }
            Loc::Slot(s) => {
                let off = self.slot_off(st, s);
                self.st(Width::X, r, SP, off)
            }
            _ => {}
        }
    }
    fn store_floc(&mut self, st: &FnState, l: Loc, d: Reg) {
        match l {
            Loc::Reg(dr) => self.a.fmov(dr, d),
            Loc::Slot(s) => {
                let off = self.slot_off(st, s);
                self.st_d(d, SP, off)
            }
            _ => {}
        }
    }
    fn commit(&mut self, st: &FnState, d: VReg, r: Reg) {
        self.store_loc(st, st.al.loc[d as usize], r);
    }
    fn commit_f(&mut self, st: &FnState, d: VReg, x: Reg) {
        self.store_floc(st, st.al.loc[d as usize], x);
    }

    /// Re-establish "narrow integers are zero-extended" after arithmetic.
    fn norm(&mut self, r: Reg, t: Ty) {
        match t {
            Ty::I8 => self.a.uxt(8, r, r),
            Ty::I16 => self.a.uxt(16, r, r),
            Ty::I32 => self.a.uxt(32, r, r),
            _ => {}
        }
    }

    // ------------------------------------------------------------- blocks
    fn block(&mut self, st: &FnState, bi: usize, next: Option<usize>) -> Result<(), String> {
        let blk = &st.f.blocks[bi];
        let n = blk.insts.len();
        let fused = st.al.fuse_cmp[bi];
        let body = if fused { n - 1 } else { n };
        for inst in &blk.insts[..body] {
            self.inst(st, inst)?;
        }
        match &blk.term {
            Term::Jump(t) => {
                if next != Some(*t as usize) {
                    self.a.b(st.labels[*t as usize]);
                }
            }
            Term::Br { cond, then_, else_ } => {
                let (t, e) = (*then_ as usize, *else_ as usize);
                if fused {
                    let Some(Inst::Cmp { op, a, b, .. }) = blk.insts.last() else { unreachable!() };
                    let c = self.compare(st, *op, *a, *b);
                    self.branch_cond(st, c, t, e, next);
                } else {
                    match self.src(st, *cond) {
                        Src::I(c) => {
                            let target = if c != 0 { t } else { e };
                            if next != Some(target) {
                                self.a.b(st.labels[target]);
                            }
                        }
                        s => {
                            let r = self.in_reg(s, T0);
                            if next == Some(t) {
                                self.a.cbz(r, st.labels[e]);
                            } else {
                                self.a.cbnz(r, st.labels[t]);
                                if next != Some(e) {
                                    self.a.b(st.labels[e]);
                                }
                            }
                        }
                    }
                }
            }
            Term::Ret(v) => {
                if let (Some(v), Some(rt)) = (v, st.f.ret) {
                    if rt.is_float() {
                        let r = self.in_freg(st, *v, 0);
                        self.a.fmov(0, r);
                    } else {
                        let s = self.src(st, *v);
                        match s {
                            Src::R(r) => self.a.mov(0, r),
                            Src::M(off) => self.ld(Width::X, 0, SP, off),
                            Src::I(i) => self.a.mov_imm(0, i),
                        }
                    }
                }
                self.epilogue(st);
            }
            Term::Unreachable => self.a.brk(0),
        }
        Ok(())
    }

    fn branch_cond(&mut self, st: &FnState, c: Cond, t: usize, e: usize, next: Option<usize>) {
        if next == Some(t) {
            self.a.b_cond(c.invert(), st.labels[e]);
        } else {
            self.a.b_cond(c, st.labels[t]);
            if next != Some(e) {
                self.a.b(st.labels[e]);
            }
        }
    }

    /// Emit a comparison; returns the condition under which `a op b` holds.
    fn compare(&mut self, st: &FnState, op: CmpOp, a: VReg, b: VReg) -> Cond {
        use CmpOp::*;
        if matches!(op, FEq | FNe | FLt | FLe | FGt | FGe) {
            let ra = self.in_freg(st, a, D0);
            let rb = self.in_freg(st, b, D1);
            self.a.fcmp(ra, rb);
            // With NaN operands every one of these conditions is false (FNe: true), as required.
            return match op {
                FEq => Cond::Eq,
                FNe => Cond::Ne,
                FLt => Cond::Mi,
                FLe => Cond::Ls,
                FGt => Cond::Gt,
                _ => Cond::Ge,
            };
        }
        let ty = st.f.vregs[a as usize];
        let signed = matches!(op, SLt | SLe | SGt | SGe);
        let sa = self.src(st, a);
        let sb = self.src(st, b);
        let narrow_signed = signed && matches!(ty, Ty::I8 | Ty::I16);
        let ra = if narrow_signed {
            let r = self.in_reg(sa, T0);
            self.a.sxt(bits_of(ty), T0, r);
            T0
        } else {
            self.in_reg(sa, T0)
        };
        // second operand: an immediate when it fits
        let imm = match sb {
            Src::I(c) => {
                let v = if narrow_signed {
                    match ty {
                        Ty::I8 => c as u8 as i8 as i64,
                        _ => c as u16 as i16 as i64,
                    }
                } else if signed && ty == Ty::I32 {
                    c as u32 as i32 as i64
                } else {
                    c
                };
                Some(v)
            }
            _ => None,
        };
        let w32 = signed && ty == Ty::I32;
        match imm {
            Some(v) if (0..4096).contains(&v) => {
                if w32 {
                    self.a.cmp32_imm(ra, v as u32)
                } else {
                    self.a.cmp_imm(ra, v as u32)
                }
            }
            Some(v) if (-4095..0).contains(&v) && !w32 => self.a.cmn_imm(ra, (-v) as u32),
            _ => {
                let rb = self.in_reg(sb, T1);
                let rb = if narrow_signed {
                    self.a.sxt(bits_of(ty), T1, rb);
                    T1
                } else {
                    rb
                };
                if w32 {
                    self.a.cmp32(ra, rb)
                } else {
                    self.a.cmp(ra, rb)
                }
            }
        }
        match op {
            Eq => Cond::Eq,
            Ne => Cond::Ne,
            SLt => Cond::Lt,
            SLe => Cond::Le,
            SGt => Cond::Gt,
            SGe => Cond::Ge,
            ULt => Cond::Lo,
            ULe => Cond::Ls,
            UGt => Cond::Hi,
            UGe => Cond::Hs,
            _ => unreachable!(),
        }
    }

    // -------------------------------------------------------- instructions
    fn inst(&mut self, st: &FnState, inst: &Inst) -> Result<(), String> {
        match inst {
            Inst::IConst { dst, val } => {
                if matches!(st.al.loc[*dst as usize], Loc::Const | Loc::Dead) {
                    return Ok(());
                }
                let v = norm_const(st.f.vregs[*dst as usize], *val);
                let rd = self.def_reg(st, *dst);
                self.a.mov_imm(rd, v);
                self.commit(st, *dst, rd);
            }
            Inst::FConst { dst, val } => {
                if matches!(st.al.loc[*dst as usize], Loc::Const | Loc::Dead) {
                    return Ok(());
                }
                let rd = self.def_freg(st, *dst);
                self.load_fbits(rd, val.to_bits());
                self.commit_f(st, *dst, rd);
            }
            Inst::Copy { dst, src } => {
                if st.f.vregs[*dst as usize].is_float() {
                    let r = self.in_freg(st, *src, D0);
                    self.commit_f(st, *dst, r);
                } else {
                    let s = self.src(st, *src);
                    match st.al.loc[*dst as usize] {
                        Loc::Dead | Loc::Const => {}
                        Loc::Reg(rd) => match s {
                            Src::R(r) => self.a.mov(rd, r),
                            Src::M(off) => self.ld(Width::X, rd, SP, off),
                            Src::I(i) => self.a.mov_imm(rd, i),
                        },
                        l @ Loc::Slot(_) => {
                            let r = self.in_reg(s, T0);
                            self.store_loc(st, l, r);
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
                let c = self.compare(st, *op, *a, *b);
                let rd = self.def_reg(st, *dst);
                self.a.cset(rd, c);
                self.commit(st, *dst, rd);
            }
            Inst::Un { dst, op, a } => self.un(st, *dst, *op, *a),
            Inst::Cast { dst, op, src } => self.cast(st, *dst, *op, *src),
            Inst::Load { dst, base, off, .. } => {
                let rb = self.in_reg(self.src(st, *base), T0);
                let t = st.f.vregs[*dst as usize];
                if t.is_float() {
                    let rd = self.def_freg(st, *dst);
                    self.ld_d(rd, rb, *off);
                    self.commit_f(st, *dst, rd);
                } else {
                    let rd = self.def_reg(st, *dst);
                    self.ld(width_of(t), rd, rb, *off);
                    self.commit(st, *dst, rd);
                }
            }
            Inst::Store { base, off, val, .. } => {
                let rb = self.in_reg(self.src(st, *base), T0);
                let t = st.f.vregs[*val as usize];
                if t.is_float() {
                    let rv = self.in_freg(st, *val, D0);
                    self.st_d(rv, rb, *off);
                } else {
                    let rv = self.in_reg(self.src(st, *val), T2);
                    self.st(width_of(t), rv, rb, *off);
                }
            }
            Inst::LoadIdx { dst, base, index, scale, off, .. } => {
                let rb = self.in_reg(self.src(st, *base), T0);
                let ri = self.in_reg(self.src(st, *index), T1);
                let t = st.f.vregs[*dst as usize];
                let w = if t.is_float() { Width::X } else { width_of(t) };
                let (b, o, rest) = self.indexed(rb, ri, *scale, *off, w);
                if t.is_float() {
                    let rd = self.def_freg(st, *dst);
                    match rest {
                        Some(r) => self.a.ldr_d_reg(rd, b, r, *scale != 0),
                        None => self.ld_d(rd, b, o),
                    }
                    self.commit_f(st, *dst, rd);
                } else {
                    let rd = self.def_reg(st, *dst);
                    match rest {
                        Some(r) => self.a.ldr_reg(w, rd, b, r, *scale != 0),
                        None => self.ld(w, rd, b, o),
                    }
                    self.commit(st, *dst, rd);
                }
            }
            Inst::StoreIdx { base, index, scale, off, val, .. } => {
                let rb = self.in_reg(self.src(st, *base), T0);
                let ri = self.in_reg(self.src(st, *index), T1);
                let t = st.f.vregs[*val as usize];
                let w = if t.is_float() { Width::X } else { width_of(t) };
                let (b, o, rest) = self.indexed(rb, ri, *scale, *off, w);
                if t.is_float() {
                    let rv = self.in_freg(st, *val, D0);
                    match rest {
                        Some(r) => self.a.str_d_reg(rv, b, r, *scale != 0),
                        None => self.st_d(rv, b, o),
                    }
                } else {
                    let rv = self.in_reg(self.src(st, *val), T2);
                    match rest {
                        Some(r) => self.a.str_reg(w, rv, b, r, *scale != 0),
                        None => self.st(w, rv, b, o),
                    }
                }
            }
            Inst::Call { dst, callee, args } => self.call(st, *dst, callee, args)?,
            Inst::DataAddr { dst, data } => {
                let rd = self.def_reg(st, *dst);
                let at = self.a.adrp_patch(rd);
                self.a.add_imm(rd, rd, 0);
                self.fix.data.push((at, *data));
                self.commit(st, *dst, rd);
            }
            Inst::StackAddr { dst, slot } => {
                let rd = self.def_reg(st, *dst);
                let off = st.slot_off[*slot as usize];
                if (0..4096).contains(&off) {
                    self.a.add_imm(rd, SP, off as u32);
                } else {
                    self.a.mov_imm(rd, off as i64);
                    self.a.add_sp_reg(rd, rd);
                }
                self.commit(st, *dst, rd);
            }
            Inst::Select { dst, cond, a, b } => self.select(st, *dst, *cond, *a, *b),
        }
        Ok(())
    }

    /// Choose the addressing form for `[base + (index << scale) + off]` of the given access width.
    /// Returns (base register, immediate offset, Some(index register) for the register-offset form).
    fn indexed(&mut self, rb: Reg, ri: Reg, scale: u8, off: i32, w: Width) -> (Reg, i32, Option<Reg>) {
        let log2 = match w {
            Width::B => 0,
            Width::H => 1,
            Width::W => 2,
            Width::X => 3,
        };
        if off == 0 && (scale == 0 || scale as u32 == log2) {
            return (rb, 0, Some(ri));
        }
        // base + (index << scale) into T2, then the immediate offset
        self.a.add_lsl(T2, rb, ri, scale as u32);
        (T2, off, None)
    }

    fn ibin(&mut self, st: &FnState, dst: VReg, op: BinOp, a: VReg, b: VReg) {
        let ty = st.f.vregs[dst as usize];
        match op {
            BinOp::SDiv | BinOp::UDiv | BinOp::SRem | BinOp::URem => return self.divrem(st, dst, op, a, b),
            BinOp::Shl | BinOp::SShr | BinOp::UShr => return self.shift(st, dst, op, a, b),
            _ => {}
        }
        let ra = self.in_reg(self.src(st, a), T0);
        let rd = self.def_reg(st, dst);
        let sb = self.src(st, b);
        let imm_ok = |c: i64| (0..4096).contains(&c);
        match (op, sb) {
            (BinOp::Add, Src::I(c)) if imm_ok(c) => self.a.add_imm(rd, ra, c as u32),
            (BinOp::Sub, Src::I(c)) if imm_ok(c) => self.a.sub_imm(rd, ra, c as u32),
            (BinOp::And, Src::I(c)) if self.a_and_imm(rd, ra, c as u64) => {}
            (BinOp::Or, Src::I(c)) if self.a.orr_imm(rd, ra, c as u64) => {}
            (BinOp::Xor, Src::I(c)) if self.a.eor_imm(rd, ra, c as u64) => {}
            _ => {
                let rb = self.in_reg(sb, T1);
                match op {
                    BinOp::Add => self.a.add(rd, ra, rb),
                    BinOp::Sub => self.a.sub(rd, ra, rb),
                    BinOp::Mul => self.a.mul(rd, ra, rb),
                    BinOp::And => self.a.and(rd, ra, rb),
                    BinOp::Or => self.a.orr(rd, ra, rb),
                    BinOp::Xor => self.a.eor(rd, ra, rb),
                    _ => unreachable!(),
                }
            }
        }
        if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) {
            self.norm(rd, ty);
        }
        self.commit(st, dst, rd);
    }

    fn a_and_imm(&mut self, rd: Reg, ra: Reg, c: u64) -> bool {
        self.a.and_imm(rd, ra, c)
    }

    fn divrem(&mut self, st: &FnState, dst: VReg, op: BinOp, a: VReg, b: VReg) {
        let ty = st.f.vregs[dst as usize];
        let signed = matches!(op, BinOp::SDiv | BinOp::SRem);
        let rem = matches!(op, BinOp::SRem | BinOp::URem);
        let mut ra = self.in_reg(self.src(st, a), T0);
        let mut rb = self.in_reg(self.src(st, b), T1);
        if signed && ty != Ty::I64 {
            // narrow signed values are zero-extended in registers: sign-extend, divide at 64 bits
            self.a.sxt(bits_of(ty), T0, ra);
            self.a.sxt(bits_of(ty), T1, rb);
            ra = T0;
            rb = T1;
        }
        let rd = self.def_reg(st, dst);
        // (The hardware returns 0 for division by zero; Flash checks for zero before dividing.)
        if signed {
            self.a.sdiv(T2, ra, rb);
        } else {
            self.a.udiv(T2, ra, rb);
        }
        if rem {
            self.a.msub(rd, T2, rb, ra);
        } else {
            self.a.mov(rd, T2);
        }
        if signed && ty != Ty::I64 {
            self.norm(rd, ty);
        }
        self.commit(st, dst, rd);
    }

    fn shift(&mut self, st: &FnState, dst: VReg, op: BinOp, a: VReg, b: VReg) {
        let ty = st.f.vregs[dst as usize];
        let bits = bits_of(ty);
        let mut ra = self.in_reg(self.src(st, a), T0);
        let rd = self.def_reg(st, dst);
        if op == BinOp::SShr && ty != Ty::I64 {
            self.a.sxt(bits, T0, ra);
            ra = T0;
        }
        match self.src(st, b) {
            Src::I(n) => {
                let n = (n as u64 % bits as u64) as u32;
                if n == 0 {
                    self.a.mov(rd, ra);
                } else {
                    match op {
                        BinOp::Shl => self.a.lsl_imm(rd, ra, n),
                        BinOp::UShr => self.a.lsr_imm(rd, ra, n),
                        _ => self.a.asr_imm(rd, ra, n),
                    }
                }
            }
            sb => {
                let mut rb = self.in_reg(sb, T1);
                if bits < 64 {
                    // shift counts are taken modulo the operand width
                    let ok = self.a.and_imm(T1, rb, (bits - 1) as u64);
                    debug_assert!(ok);
                    rb = T1;
                }
                match op {
                    BinOp::Shl => self.a.lslv(rd, ra, rb),
                    BinOp::UShr => self.a.lsrv(rd, ra, rb),
                    _ => self.a.asrv(rd, ra, rb),
                }
            }
        }
        if ty != Ty::I64 {
            self.norm(rd, ty);
        }
        self.commit(st, dst, rd);
    }

    fn fbin(&mut self, st: &FnState, dst: VReg, op: BinOp, a: VReg, b: VReg) {
        let ra = self.in_freg(st, a, D0);
        let rb = self.in_freg(st, b, D1);
        let rd = self.def_freg(st, dst);
        match op {
            BinOp::FAdd => self.a.fadd(rd, ra, rb),
            BinOp::FSub => self.a.fsub(rd, ra, rb),
            BinOp::FMul => self.a.fmul(rd, ra, rb),
            BinOp::FDiv => self.a.fdiv(rd, ra, rb),
            _ => unreachable!("integer op on floats"),
        }
        self.commit_f(st, dst, rd);
    }

    fn un(&mut self, st: &FnState, dst: VReg, op: UnOp, a: VReg) {
        let ty = st.f.vregs[dst as usize];
        match op {
            UnOp::Neg | UnOp::Not => {
                let ra = self.in_reg(self.src(st, a), T0);
                let rd = self.def_reg(st, dst);
                if op == UnOp::Neg {
                    self.a.neg(rd, ra);
                } else {
                    self.a.mvn(rd, ra);
                }
                self.norm(rd, ty);
                self.commit(st, dst, rd);
            }
            _ => {
                let ra = self.in_freg(st, a, D0);
                let rd = self.def_freg(st, dst);
                match op {
                    UnOp::FNeg => self.a.fneg(rd, ra),
                    UnOp::FAbs => self.a.fabs(rd, ra),
                    UnOp::FSqrt => self.a.fsqrt(rd, ra),
                    UnOp::FFloor => self.a.frintm(rd, ra),
                    UnOp::FCeil => self.a.frintp(rd, ra),
                    _ => unreachable!(),
                }
                self.commit_f(st, dst, rd);
            }
        }
    }

    fn cast(&mut self, st: &FnState, dst: VReg, op: CastOp, src: VReg) {
        let dt = st.f.vregs[dst as usize];
        let stp = st.f.vregs[src as usize];
        match op {
            CastOp::ZExt => {
                let r = self.in_reg(self.src(st, src), T0);
                self.commit(st, dst, r);
            }
            CastOp::SExt => {
                let r = self.in_reg(self.src(st, src), T0);
                let rd = self.def_reg(st, dst);
                self.a.sxt(bits_of(stp), rd, r);
                self.norm(rd, dt);
                self.commit(st, dst, rd);
            }
            CastOp::Trunc => {
                let r = self.in_reg(self.src(st, src), T0);
                let rd = self.def_reg(st, dst);
                self.a.mov(rd, r);
                self.norm(rd, dt);
                self.commit(st, dst, rd);
            }
            CastOp::SToF | CastOp::UToF => {
                let mut r = self.in_reg(self.src(st, src), T0);
                if op == CastOp::SToF && stp != Ty::I64 {
                    self.a.sxt(bits_of(stp), T0, r);
                    r = T0;
                }
                let rd = self.def_freg(st, dst);
                if op == CastOp::SToF {
                    self.a.scvtf(rd, r);
                } else {
                    self.a.ucvtf(rd, r);
                }
                self.commit_f(st, dst, rd);
            }
            CastOp::FToS | CastOp::FToU => {
                // fcvtzs / fcvtzu saturate and turn NaN into 0, exactly Flash's semantics
                let r = self.in_freg(st, src, D0);
                let rd = self.def_reg(st, dst);
                if op == CastOp::FToS {
                    self.a.fcvtzs(rd, r);
                } else {
                    self.a.fcvtzu(rd, r);
                }
                if dt != Ty::I64 {
                    let bits = bits_of(dt);
                    if op == CastOp::FToS {
                        let (lo, hi) = (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1);
                        self.a.mov_imm(T1, hi);
                        self.a.cmp(rd, T1);
                        self.a.csel(rd, T1, rd, Cond::Gt);
                        self.a.mov_imm(T1, lo);
                        self.a.cmp(rd, T1);
                        self.a.csel(rd, T1, rd, Cond::Lt);
                    } else {
                        self.a.mov_imm(T1, (1i64 << bits) - 1);
                        self.a.cmp(rd, T1);
                        self.a.csel(rd, T1, rd, Cond::Hi);
                    }
                    self.norm(rd, dt);
                }
                self.commit(st, dst, rd);
            }
            CastOp::FBitsToI => {
                let r = self.in_freg(st, src, D0);
                let rd = self.def_reg(st, dst);
                self.a.fmov_to_x(rd, r);
                self.commit(st, dst, rd);
            }
            CastOp::IBitsToF => {
                let r = self.in_reg(self.src(st, src), T0);
                let rd = self.def_freg(st, dst);
                self.a.fmov_from_x(rd, r);
                self.commit_f(st, dst, rd);
            }
        }
    }

    fn select(&mut self, st: &FnState, dst: VReg, cond: VReg, a: VReg, b: VReg) {
        let rc = self.in_reg(self.src(st, cond), T0);
        self.a.cmp_imm(rc, 0);
        if st.f.vregs[dst as usize].is_float() {
            let ra = self.in_freg(st, a, D0);
            let rb = self.in_freg(st, b, D1);
            let rd = self.def_freg(st, dst);
            self.a.fcsel(rd, ra, rb, Cond::Ne);
            self.commit_f(st, dst, rd);
        } else {
            let ra = self.in_reg(self.src(st, a), T1);
            let rb = self.in_reg(self.src(st, b), T2);
            let rd = self.def_reg(st, dst);
            self.a.csel(rd, ra, rb, Cond::Ne);
            self.commit(st, dst, rd);
        }
    }

    // -------------------------------------------------------------- calls
    fn call(&mut self, st: &FnState, dst: Option<VReg>, callee: &Callee, args: &[VReg]) -> Result<(), String> {
        if let Callee::Syscall = callee {
            // args[0] = number, args[1..] = up to six arguments in x0–x5
            for (i, a) in args[1..].iter().enumerate() {
                self.move_arg_int(st, i as Reg, *a);
            }
            let Src::I(num) = self.src(st, args[0]) else { return Err("system call numbers must be constants".into()) };
            match self.target.os {
                super::Os::Linux => {
                    self.a.mov_imm(8, num);
                    self.a.svc(0);
                }
                super::Os::MacOs => {
                    // BSD syscall: number in x16, trap with svc #0x80; the carry flag signals an
                    // error with a positive errno in x0, which the runtime expects as -errno.
                    self.a.mov_imm(16, num);
                    self.a.svc(0x80);
                    let ok = self.a.new_label();
                    self.a.b_cond(Cond::Lo, ok); // carry clear: success
                    self.a.neg(0, 0);
                    self.a.bind(ok);
                }
            }
            if let Some(d) = dst {
                self.commit(st, d, 0);
            }
            return Ok(());
        }
        let tys: Vec<Ty> = args.iter().map(|a| st.f.vregs[*a as usize]).collect();
        let locs = classify(&tys);
        for (a, l) in args.iter().zip(&locs) {
            match l {
                ArgLoc::Int(r) => self.move_arg_int(st, *r, *a),
                ArgLoc::Flt(r) => {
                    let s = self.in_freg(st, *a, *r);
                    self.a.fmov(*r, s);
                }
                ArgLoc::Stack(k) => {
                    if st.f.vregs[*a as usize].is_float() {
                        let s = self.in_freg(st, *a, D0);
                        self.st_d(s, SP, 8 * *k as i32);
                    } else {
                        let r = self.in_reg(self.src(st, *a), T0);
                        self.st(Width::X, r, SP, 8 * *k as i32);
                    }
                }
            }
        }
        match callee {
            Callee::Func(i) => {
                let at = self.a.bl_patch();
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
                self.commit_f(st, d, 0);
            } else {
                self.commit(st, d, 0);
            }
        }
        Ok(())
    }

    fn move_arg_int(&mut self, st: &FnState, dst: Reg, v: VReg) {
        match self.src(st, v) {
            Src::R(r) => self.a.mov(dst, r),
            Src::M(off) => self.ld(Width::X, dst, SP, off),
            Src::I(i) => self.a.mov_imm(dst, i),
        }
    }
}
