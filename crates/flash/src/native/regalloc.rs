//! Register allocation for the native back end.
//!
//! The IR is not SSA (virtual registers may be assigned several times), so allocation works on
//! live ranges computed from block-level liveness:
//!
//! 1. blocks are laid out in reverse post-order, with blocks that end in `Unreachable`
//!    (panic paths) moved to the end;
//! 2. every vreg gets a list of live ranges (with holes) and a "lives across a call" flag;
//! 3. a linear scan in order of range start assigns each vreg a register or a stack slot.
//!    A vreg whose range overlaps a call may only live in a callee-saved register; when no
//!    register is free the cheapest overlapping vreg (use count weighted by loop depth) is spilled.
//!
//! Single-definition constants are not allocated at all: the code generator rematerialises them
//! or folds them into instruction immediates.

use crate::ir::*;

use super::asm::*;

/// The registers a target offers to the allocator.
pub struct RegConfig {
    pub int_caller: &'static [Reg],
    pub int_callee: &'static [Reg],
    pub flt_caller: &'static [Reg],
    pub flt_callee: &'static [Reg],
}

/// x86-64: rax, rcx, rdx, r11 and xmm14/15 are scratch registers of the code generator.
pub const X86_64: RegConfig = RegConfig { int_caller: &INT_CALLER_SAVED, int_callee: &INT_CALLEE_SAVED, flt_caller: &FLT_CALLER_SAVED, flt_callee: &FLT_CALLEE_SAVED };

/// AArch64: x8, x16, x17 and v30/v31 are scratch registers of the code generator; x18 is reserved
/// (platform register on macOS), x29/x30 are the frame pointer and link register.
pub const AARCH64: RegConfig = RegConfig {
    int_caller: &[9, 10, 11, 12, 13, 14, 15],
    int_callee: &[19, 20, 21, 22, 23, 24, 25, 26, 27, 28],
    flt_caller: &[16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29],
    flt_callee: &[8, 9, 10, 11, 12, 13, 14, 15],
};

/// Integer registers available for allocation. RAX, RCX, RDX, R11 are scratch registers of the
/// code generator, RSP is the stack pointer.
pub const INT_CALLER_SAVED: [Reg; 5] = [RSI, RDI, R8, R9, R10];
pub const INT_CALLEE_SAVED: [Reg; 6] = [RBX, RBP, R12, R13, R14, R15];
/// XMM14 and XMM15 are scratch registers. Flash's own calling convention keeps XMM8–XMM13 intact
/// across calls (callees save what they use).
pub const FLT_CALLER_SAVED: [Reg; 8] = [0, 1, 2, 3, 4, 5, 6, 7];
pub const FLT_CALLEE_SAVED: [Reg; 6] = [8, 9, 10, 11, 12, 13];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Const {
    I(i64),
    /// Raw bits of an f64.
    F(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loc {
    Reg(Reg),
    /// Index into the spill area.
    Slot(u32),
    /// A single-definition constant (see `Allocation::consts`).
    Const,
    /// Never read: writes are discarded.
    Dead,
}

pub struct Allocation {
    /// Block layout (indices into `Function::blocks`); unreachable blocks are omitted.
    pub order: Vec<usize>,
    pub loc: Vec<Loc>,
    pub consts: Vec<Option<Const>>,
    pub saved_int: Vec<Reg>,
    pub saved_flt: Vec<Reg>,
    pub nslots: u32,
    /// Per IR block: the trailing compare may be fused into the branch (its result is dead after it).
    pub fuse_cmp: Vec<bool>,
}

pub fn is_noreturn_call(f: &Function, b: &Block) -> bool {
    let _ = f;
    matches!(b.term, Term::Unreachable)
}

pub fn for_uses(i: &Inst, mut f: impl FnMut(VReg)) {
    match i {
        Inst::IConst { .. } | Inst::FConst { .. } | Inst::DataAddr { .. } | Inst::ThreadPtr { .. } | Inst::StackAddr { .. } => {}
        Inst::Copy { src, .. } | Inst::Cast { src, .. } => f(*src),
        Inst::Un { a, .. } => f(*a),
        Inst::Bin { a, b, .. } | Inst::Cmp { a, b, .. } => {
            f(*a);
            f(*b);
        }
        Inst::Load { base, .. } => f(*base),
        Inst::LoadIdx { base, index, .. } => {
            f(*base);
            f(*index);
        }
        Inst::Store { base, val, .. } => {
            f(*base);
            f(*val);
        }
        Inst::StoreIdx { base, index, val, .. } => {
            f(*base);
            f(*index);
            f(*val);
        }
        Inst::Call { callee, args, .. } => {
            if let Callee::Indirect(v) = callee {
                f(*v);
            }
            for a in args {
                f(*a);
            }
        }
        Inst::Select { cond, a, b, .. } => {
            f(*cond);
            f(*a);
            f(*b);
        }
    }
}

pub fn def_of(i: &Inst) -> Option<VReg> {
    match i {
        Inst::IConst { dst, .. }
        | Inst::FConst { dst, .. }
        | Inst::Copy { dst, .. }
        | Inst::Bin { dst, .. }
        | Inst::Cmp { dst, .. }
        | Inst::Un { dst, .. }
        | Inst::Cast { dst, .. }
        | Inst::Load { dst, .. }
        | Inst::LoadIdx { dst, .. }
        | Inst::DataAddr { dst, .. }
        | Inst::ThreadPtr { dst }
        | Inst::StackAddr { dst, .. }
        | Inst::Select { dst, .. } => Some(*dst),
        Inst::Call { dst, .. } => *dst,
        Inst::Store { .. } | Inst::StoreIdx { .. } => None,
    }
}

fn term_uses(t: &Term) -> Option<VReg> {
    match t {
        Term::Br { cond, .. } => Some(*cond),
        Term::Ret(v) => *v,
        _ => None,
    }
}

pub fn successors(t: &Term) -> Vec<usize> {
    match t {
        Term::Jump(b) => vec![*b as usize],
        Term::Br { then_, else_, .. } => vec![*then_ as usize, *else_ as usize],
        _ => vec![],
    }
}

/// Fixed-size bit set.
#[derive(Clone)]
struct Bits(Vec<u64>);
impl Bits {
    fn new(n: usize) -> Bits {
        Bits(vec![0; n.div_ceil(64)])
    }
    fn set(&mut self, i: usize) {
        self.0[i / 64] |= 1 << (i % 64);
    }
    fn clear(&mut self, i: usize) {
        self.0[i / 64] &= !(1 << (i % 64));
    }
    fn get(&self, i: usize) -> bool {
        self.0[i / 64] >> (i % 64) & 1 != 0
    }
    fn or_with(&mut self, o: &Bits) -> bool {
        let mut ch = false;
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            let n = *a | *b;
            ch |= n != *a;
            *a = n;
        }
        ch
    }
    fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().enumerate().flat_map(|(wi, w)| {
            let mut w = *w;
            std::iter::from_fn(move || {
                if w == 0 {
                    None
                } else {
                    let t = w.trailing_zeros() as usize;
                    w &= w - 1;
                    Some(wi * 64 + t)
                }
            })
        })
    }
}

fn layout(f: &Function) -> Vec<usize> {
    let n = f.blocks.len();
    let mut seen = vec![false; n];
    let mut post = vec![];
    // Iterative DFS for post-order.
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    seen[0] = true;
    while let Some((b, i)) = stack.pop() {
        let succ = successors(&f.blocks[b].term);
        if i < succ.len() {
            stack.push((b, i + 1));
            let s = succ[i];
            if !seen[s] {
                seen[s] = true;
                stack.push((s, 0));
            }
        } else {
            post.push(b);
        }
    }
    post.reverse();
    // Panic paths go last so that they do not interrupt the hot code.
    let (hot, cold): (Vec<usize>, Vec<usize>) = post.into_iter().partition(|b| !is_noreturn_call(f, &f.blocks[*b]));
    let mut order = hot;
    // The entry block must stay first.
    order.extend(cold);
    order
}

type Ranges = Vec<(u32, u32)>;

fn overlaps(a: &Ranges, b: &Ranges) -> bool {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].1 <= b[j].0 {
            i += 1;
        } else if b[j].1 <= a[i].0 {
            j += 1;
        } else {
            return true;
        }
    }
    false
}

pub fn allocate(f: &Function, cfg: &RegConfig) -> Allocation {
    let nv = f.vregs.len();
    let order = layout(f);
    let nb = order.len();
    let mut idx_of = vec![usize::MAX; f.blocks.len()];
    for (i, b) in order.iter().enumerate() {
        idx_of[*b] = i;
    }

    // ---- constants and use counts
    let mut defs = vec![0u32; nv];
    let mut uses = vec![0u32; nv];
    let mut const_def: Vec<Option<Const>> = vec![None; nv];
    for b in &order {
        let blk = &f.blocks[*b];
        for inst in &blk.insts {
            if let Some(d) = def_of(inst) {
                defs[d as usize] += 1;
                match inst {
                    Inst::IConst { dst, val } => const_def[*dst as usize] = Some(Const::I(norm_const(f.vregs[*dst as usize], *val))),
                    Inst::FConst { dst, val } => const_def[*dst as usize] = Some(Const::F(val.to_bits())),
                    _ => {}
                }
            }
            for_uses(inst, |u| uses[u as usize] += 1);
        }
        if let Some(u) = term_uses(&blk.term) {
            uses[u as usize] += 1;
        }
    }
    let mut consts: Vec<Option<Const>> = vec![None; nv];
    for v in f.params.len()..nv {
        if defs[v] == 1 {
            consts[v] = const_def[v];
        }
    }

    // ---- block liveness
    let mut use_b: Vec<Bits> = vec![Bits::new(nv); nb];
    let mut def_b: Vec<Bits> = vec![Bits::new(nv); nb];
    for (bi, b) in order.iter().enumerate() {
        let blk = &f.blocks[*b];
        for inst in &blk.insts {
            for_uses(inst, |u| {
                if consts[u as usize].is_none() && !def_b[bi].get(u as usize) {
                    use_b[bi].set(u as usize);
                }
            });
            if let Some(d) = def_of(inst) {
                def_b[bi].set(d as usize);
            }
        }
        if let Some(u) = term_uses(&blk.term) {
            if consts[u as usize].is_none() && !def_b[bi].get(u as usize) {
                use_b[bi].set(u as usize);
            }
        }
    }
    let succs: Vec<Vec<usize>> = order.iter().map(|b| successors(&f.blocks[*b].term).into_iter().map(|s| idx_of[s]).collect()).collect();
    let mut live_in: Vec<Bits> = use_b.clone();
    let mut live_out: Vec<Bits> = vec![Bits::new(nv); nb];
    loop {
        let mut changed = false;
        for bi in (0..nb).rev() {
            let mut out = Bits::new(nv);
            for s in &succs[bi] {
                out.or_with(&live_in[*s]);
            }
            // in = use | (out - def)
            let mut inn = use_b[bi].clone();
            for (w, (o, d)) in inn.0.iter_mut().zip(out.0.iter().zip(&def_b[bi].0)) {
                *w |= o & !d;
            }
            if inn.0 != live_in[bi].0 {
                live_in[bi] = inn;
                changed = true;
            }
            live_out[bi] = out;
        }
        if !changed {
            break;
        }
    }

    // ---- loop depth (back edges in layout order) and block positions
    let mut depth = vec![0u32; nb];
    for bi in 0..nb {
        for s in &succs[bi] {
            if *s <= bi {
                for d in depth.iter_mut().take(bi + 1).skip(*s) {
                    *d += 1;
                }
            }
        }
    }
    let mut bstart = vec![0u32; nb];
    let mut pos = 2u32;
    for bi in 0..nb {
        bstart[bi] = pos;
        pos += 2 * (f.blocks[order[bi]].insts.len() as u32 + 1);
    }

    // ---- live ranges and call crossing
    let mut ranges: Vec<Ranges> = vec![vec![]; nv];
    let mut crossing = Bits::new(nv);
    let mut weight = vec![0f64; nv];
    let mut hint: Vec<Option<VReg>> = vec![None; nv];
    fn add_range(r: &mut Ranges, s: u32, e: u32) {
        if let Some(last) = r.last_mut() {
            if last.0 <= e {
                last.0 = last.0.min(s);
                last.1 = last.1.max(e);
                return;
            }
        }
        r.push((s, e));
    }
    for bi in (0..nb).rev() {
        let blk = &f.blocks[order[bi]];
        let end = bstart[bi] + 2 * (blk.insts.len() as u32 + 1);
        let w = 8f64.powi(depth[bi].min(5) as i32);
        let mut live = live_out[bi].clone();
        for v in live.iter() {
            add_range(&mut ranges[v], bstart[bi], end);
        }
        let n = blk.insts.len();
        // terminator
        let tp = bstart[bi] + 2 * n as u32;
        if let Some(u) = term_uses(&blk.term) {
            if consts[u as usize].is_none() {
                weight[u as usize] += w;
                if !live.get(u as usize) {
                    add_range(&mut ranges[u as usize], bstart[bi], tp + 1);
                    live.set(u as usize);
                }
            }
        }
        for k in (0..n).rev() {
            let inst = &blk.insts[k];
            let p = bstart[bi] + 2 * k as u32;
            if let Some(d) = def_of(inst) {
                let du = d as usize;
                if consts[du].is_none() {
                    weight[du] += w;
                    if live.get(du) {
                        if let Some(last) = ranges[du].last_mut() {
                            last.0 = p + 1;
                        }
                        live.clear(du);
                    } else {
                        add_range(&mut ranges[du], p + 1, p + 2);
                    }
                }
            }
            // A system call clobbers the argument registers like a call does (rcx and r11 are scratch).
            if matches!(inst, Inst::Call { .. }) {
                crossing.or_with(&live);
            }
            for_uses(inst, |u| {
                let uu = u as usize;
                if consts[uu].is_some() {
                    return;
                }
                weight[uu] += w;
                if !live.get(uu) {
                    add_range(&mut ranges[uu], bstart[bi], p + 1);
                    live.set(uu);
                }
            });
            match inst {
                Inst::Copy { dst, src } if hint[*dst as usize].is_none() => hint[*dst as usize] = Some(*src),
                Inst::Bin { dst, a, .. } if hint[*dst as usize].is_none() => hint[*dst as usize] = Some(*a),
                _ => {}
            }
        }
    }
    // Parameters are defined at function entry (position 1).
    for p in 0..f.params.len() {
        if let Some(first) = ranges[p].last_mut() {
            first.0 = 1;
        }
    }
    for r in ranges.iter_mut() {
        r.reverse();
    }

    // ---- linear scan
    let mut loc = vec![Loc::Dead; nv];
    for v in 0..nv {
        if consts[v].is_some() {
            loc[v] = Loc::Const;
        }
    }
    let mut vregs: Vec<usize> = (0..nv).filter(|v| consts[*v].is_none() && !ranges[*v].is_empty()).collect();
    vregs.sort_by_key(|v| (ranges[*v][0].0, *v));

    // assigned[class][reg] -> vregs currently holding that register (pruned lazily)
    let mut by_reg: Vec<Vec<usize>> = vec![vec![]; 64];
    let mut nslots = 0u32;
    for &v in &vregs {
        let flt = f.vregs[v].is_float();
        let cross = crossing.get(v);
        let start = ranges[v][0].0;
        let candidates: Vec<Reg> = if flt {
            if cross {
                cfg.flt_callee.to_vec()
            } else {
                cfg.flt_caller.iter().chain(cfg.flt_callee.iter()).copied().collect()
            }
        } else if cross {
            cfg.int_callee.to_vec()
        } else {
            cfg.int_caller.iter().chain(cfg.int_callee.iter()).copied().collect()
        };
        let key = |r: Reg| if flt { 32 + r as usize } else { r as usize };
        // Prefer the register of a coalescing hint.
        let mut cands = candidates.clone();
        if let Some(h) = hint[v] {
            if let Loc::Reg(hr) = loc[h as usize] {
                if let Some(i) = cands.iter().position(|r| *r == hr) {
                    let r = cands.remove(i);
                    cands.insert(0, r);
                }
            }
        }
        let mut chosen = None;
        for &r in &cands {
            let list = &mut by_reg[key(r)];
            list.retain(|o| ranges[*o].last().is_some_and(|l| l.1 > start));
            if !list.iter().any(|o| loc[*o] == Loc::Reg(r) && overlaps(&ranges[*o], &ranges[v])) {
                chosen = Some(r);
                break;
            }
        }
        if let Some(r) = chosen {
            loc[v] = Loc::Reg(r);
            by_reg[key(r)].push(v);
            continue;
        }
        // No free register: evict the cheapest set of conflicting vregs if that is cheaper than spilling v.
        let mut best: Option<(Reg, f64, Vec<usize>)> = None;
        for &r in &cands {
            let conflicts: Vec<usize> = by_reg[key(r)]
                .iter()
                .copied()
                .filter(|o| loc[*o] == Loc::Reg(r) && overlaps(&ranges[*o], &ranges[v]))
                .collect();
            let cost: f64 = conflicts.iter().map(|o| weight[*o]).sum();
            if best.as_ref().is_none_or(|b| cost < b.1) {
                best = Some((r, cost, conflicts));
            }
        }
        match best {
            Some((r, cost, conflicts)) if cost < weight[v] => {
                for o in conflicts {
                    loc[o] = Loc::Slot(nslots);
                    nslots += 1;
                }
                loc[v] = Loc::Reg(r);
                by_reg[key(r)].push(v);
            }
            _ => {
                loc[v] = Loc::Slot(nslots);
                nslots += 1;
            }
        }
    }

    let mut saved_int = vec![];
    let mut saved_flt = vec![];
    for (v, l) in loc.iter().enumerate() {
        if let Loc::Reg(r) = l {
            if f.vregs[v].is_float() {
                if cfg.flt_callee.contains(r) && !saved_flt.contains(r) {
                    saved_flt.push(*r);
                }
            } else if cfg.int_callee.contains(r) && !saved_int.contains(r) {
                saved_int.push(*r);
            }
        }
    }
    saved_int.sort();
    saved_flt.sort();
    let mut fuse_cmp = vec![false; f.blocks.len()];
    for (bi, b) in order.iter().enumerate() {
        let blk = &f.blocks[*b];
        if let (Some(Inst::Cmp { dst, .. }), Term::Br { cond, .. }) = (blk.insts.last(), &blk.term) {
            fuse_cmp[*b] = dst == cond && consts[*dst as usize].is_none() && !live_out[bi].get(*dst as usize);
        }
    }
    Allocation { order, loc, consts, saved_int, saved_flt, nslots, fuse_cmp }
}

/// Constants are kept in their canonical in-register form: narrow integers zero-extended.
pub fn norm_const(t: Ty, val: i64) -> i64 {
    match t {
        Ty::I8 => val as u8 as i64,
        Ty::I16 => val as u16 as i64,
        Ty::I32 => val as u32 as i64,
        _ => val,
    }
}
