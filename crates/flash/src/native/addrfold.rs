//! Address folding, the last IR pass before code generation.
//!
//! `p = base + (i * 8); x = load [p + off]` becomes a single indexed load, which the code
//! generator emits as one `mov x, [base + i*8 + off]`. The address computation disappears when
//! nothing else uses it; the operands stay live up to the memory access, which register
//! allocation then sees as ordinary uses.

use std::collections::HashMap;

use crate::ir::*;

use super::regalloc::{def_of, for_uses};

fn scale_of(op: BinOp, c: i64) -> Option<u8> {
    match (op, c) {
        (BinOp::Mul, 1) => Some(0),
        (BinOp::Mul, 2) => Some(1),
        (BinOp::Mul, 4) => Some(2),
        (BinOp::Mul, 8) => Some(3),
        (BinOp::Shl, 0..=3) => Some(c as u8),
        _ => None,
    }
}

pub fn run(f: &mut Function) {
    let nv = f.vregs.len();
    let mut defs = vec![0u32; nv];
    let mut uses = vec![0u32; nv];
    let mut const_val: HashMap<VReg, i64> = HashMap::new();
    for d in defs.iter_mut().take(f.params.len()) {
        *d += 1;
    }
    for b in &f.blocks {
        for i in &b.insts {
            if let Some(d) = def_of(i) {
                defs[d as usize] += 1;
            }
            for_uses(i, |u| uses[u as usize] += 1);
        }
        match &b.term {
            Term::Br { cond, .. } => uses[*cond as usize] += 1,
            Term::Ret(Some(v)) => uses[*v as usize] += 1,
            _ => {}
        }
    }
    for b in &f.blocks {
        for i in &b.insts {
            if let Inst::IConst { dst, val } = i {
                if defs[*dst as usize] == 1 {
                    const_val.insert(*dst, *val);
                }
            }
        }
    }
    let single = |v: VReg| defs[v as usize] == 1 && uses[v as usize] == 1;
    let is_i64 = |v: VReg| f.vregs[v as usize] == Ty::I64;

    let mut new_blocks: Vec<Vec<Inst>> = Vec::with_capacity(f.blocks.len());
    for b in &f.blocks {
        let insts = &b.insts;
        let mut removed = vec![false; insts.len()];
        let mut rewritten: Vec<Option<Inst>> = vec![None; insts.len()];
        let mut last_def: HashMap<VReg, usize> = HashMap::new();
        for (k, inst) in insts.iter().enumerate() {
            let mem = match inst {
                Inst::Load { base, off, .. } => Some((*base, *off)),
                Inst::Store { base, off, .. } => Some((*base, *off)),
                _ => None,
            };
            if let Some((p, off)) = mem {
                if let Some(&j) = last_def.get(&p) {
                    if single(p) && !removed[j] {
                        if let Inst::Bin { op: BinOp::Add, a, b: bb, .. } = &insts[j] {
                            if is_i64(*a) && is_i64(*bb) {
                                // Try both operand orders: one is the base, the other the (scaled) index.
                                for (base, other) in [(*a, *bb), (*bb, *a)] {
                                    if let Some(f) = fold(insts, &last_def, &single, &const_val, &removed, j, k, base, other, off) {
                                        removed[j] = true;
                                        if let Some(m) = f.removed_mul {
                                            removed[m] = true;
                                        }
                                        rewritten[k] = Some(f.inst_for(inst));
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if let Some(d) = def_of(inst) {
                last_def.insert(d, k);
            }
        }
        let mut out = Vec::with_capacity(insts.len());
        for (k, inst) in insts.iter().enumerate() {
            if removed[k] {
                continue;
            }
            out.push(rewritten[k].clone().unwrap_or_else(|| inst.clone()));
        }
        new_blocks.push(out);
    }
    for (b, insts) in f.blocks.iter_mut().zip(new_blocks) {
        b.insts = insts;
    }
}

struct Folded {
    base: VReg,
    index: Option<(VReg, u8)>,
    off: i32,
    removed_mul: Option<usize>,
}

impl Folded {
    fn inst_for(&self, original: &Inst) -> Inst {
        match (original, self.index) {
            (Inst::Load { dst, tag, .. }, Some((index, scale))) => Inst::LoadIdx { dst: *dst, base: self.base, index, scale, off: self.off, tag: *tag },
            (Inst::Store { val, tag, .. }, Some((index, scale))) => Inst::StoreIdx { base: self.base, index, scale, off: self.off, val: *val, tag: *tag },
            (Inst::Load { dst, tag, .. }, None) => Inst::Load { dst: *dst, base: self.base, off: self.off, tag: *tag },
            (Inst::Store { val, tag, .. }, None) => Inst::Store { base: self.base, off: self.off, val: *val, tag: *tag },
            _ => unreachable!(),
        }
    }
}

/// Is `v` assigned anywhere in `insts[from..to]`?
fn redefined(insts: &[Inst], from: usize, to: usize, v: VReg) -> bool {
    insts[from..to].iter().any(|i| def_of(i) == Some(v))
}

#[allow(clippy::too_many_arguments)]
fn fold(
    insts: &[Inst],
    last_def: &HashMap<VReg, usize>,
    single: &dyn Fn(VReg) -> bool,
    const_val: &HashMap<VReg, i64>,
    removed: &[bool],
    j: usize,
    k: usize,
    base: VReg,
    other: VReg,
    off: i32,
) -> Option<Folded> {
    // The base register must still hold the same value at the memory access.
    if const_val.contains_key(&base) || redefined(insts, j + 1, k, base) {
        return None;
    }
    // `base + constant`: just a displacement.
    if let Some(&c) = const_val.get(&other) {
        let total = i64::from(off) + c;
        if let Ok(total) = i32::try_from(total) {
            return Some(Folded { base, index: None, off: total, removed_mul: None });
        }
        return None;
    }
    // `base + (x << s)` or `base + x * {1,2,4,8}`
    if let Some(&m) = last_def.get(&other) {
        if single(other) && !removed[m] {
            let scaled = match &insts[m] {
                Inst::Bin { op, a, b, .. } => match (const_val.get(b), const_val.get(a)) {
                    (Some(&c), _) => scale_of(*op, c).map(|s| (*a, s)),
                    (None, Some(&c)) if matches!(op, BinOp::Mul) => scale_of(*op, c).map(|s| (*b, s)),
                    _ => None,
                },
                _ => None,
            };
            if let Some((x, s)) = scaled {
                if !const_val.contains_key(&x) && insts_ty_ok(insts, x) && !redefined(insts, m + 1, k, x) {
                    return Some(Folded { base, index: Some((x, s)), off, removed_mul: Some(m) });
                }
            }
        }
    }
    // plain `base + index`
    if !redefined(insts, j + 1, k, other) && !const_val.contains_key(&other) {
        return Some(Folded { base, index: Some((other, 0)), off, removed_mul: None });
    }
    None
}

fn insts_ty_ok(_insts: &[Inst], _x: VReg) -> bool {
    true
}
