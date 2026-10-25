//! IR optimisation passes: constant folding / propagation, copy propagation, dead-code
//! elimination, control-flow simplification and (in release builds) function inlining.
//!
//! All passes work on the backend-independent IR and preserve behaviour exactly, including
//! runtime checks (bounds, division by zero) which are separate branches in the IR.

use std::collections::HashMap;

use crate::ir::*;

pub fn optimise(m: &mut Module, release: bool) {
    for f in &mut m.funcs {
        simplify(f);
    }
    if release {
        inline_small_functions(m);
        for f in &mut m.funcs {
            simplify(f);
        }
        let ntags = m.tags.len();
        for f in &mut m.funcs {
            let mut c = crate::gvn::gvn(f, ntags);
            if crate::licm::licm(f) {
                crate::gvn::gvn(f, ntags);
                c = true;
            }
            if c {
                simplify(f);
            }
        }
    }
    for f in &mut m.funcs {
        if rotate_loops(f) {
            simplify(f);
        }
    }
    remove_unused_functions(m);
}

fn simplify(f: &mut Function) {
    for _ in 0..8 {
        let mut changed = false;
        changed |= fold_constants(f);
        changed |= local_copy_propagation(f);
        changed |= fold_copies_into_defs(f);
        changed |= thread_known_conditions(f);
        changed |= simplify_cfg(f);
        changed |= dead_code(f);
        changed |= dead_defs(f);
        if !changed {
            break;
        }
    }
}

// ----------------------------------------------------------------- helpers
fn def_of(i: &Inst) -> Option<VReg> {
    match i {
        Inst::IConst { dst, .. }
        | Inst::FConst { dst, .. }
        | Inst::Copy { dst, .. }
        | Inst::Bin { dst, .. }
        | Inst::Cmp { dst, .. }
        | Inst::Un { dst, .. }
        | Inst::Cast { dst, .. }
        | Inst::Load { dst, .. }
        | Inst::DataAddr { dst, .. }
        | Inst::StackAddr { dst, .. }
        | Inst::Select { dst, .. } => Some(*dst),
        Inst::Call { dst, .. } => *dst,
        Inst::Store { .. } => None,
    }
}

fn for_uses(i: &mut Inst, mut f: impl FnMut(&mut VReg)) {
    match i {
        Inst::IConst { .. } | Inst::FConst { .. } | Inst::DataAddr { .. } | Inst::StackAddr { .. } => {}
        Inst::Copy { src, .. } | Inst::Cast { src, .. } => f(src),
        Inst::Un { a, .. } => f(a),
        Inst::Bin { a, b, .. } | Inst::Cmp { a, b, .. } => {
            f(a);
            f(b);
        }
        Inst::Load { base, .. } => f(base),
        Inst::Store { base, val, .. } => {
            f(base);
            f(val);
        }
        Inst::Call { callee, args, .. } => {
            if let Callee::Indirect(v) = callee {
                f(v);
            }
            for a in args {
                f(a);
            }
        }
        Inst::Select { cond, a, b, .. } => {
            f(cond);
            f(a);
            f(b);
        }
    }
}

fn term_uses(t: &mut Term, mut f: impl FnMut(&mut VReg)) {
    match t {
        Term::Br { cond, .. } => f(cond),
        Term::Ret(Some(v)) => f(v),
        _ => {}
    }
}

fn is_pure(i: &Inst) -> bool {
    !matches!(i, Inst::Store { .. } | Inst::Call { .. })
}

/// Canonical form of an integer constant of type `ty` (sign-extended from its width).
fn canon(ty: Ty, v: i64) -> i64 {
    match ty {
        Ty::I8 => v as i8 as i64,
        Ty::I16 => v as i16 as i64,
        Ty::I32 => v as i32 as i64,
        _ => v,
    }
}

fn ubits(ty: Ty, v: i64) -> u64 {
    match ty {
        Ty::I8 => v as u8 as u64,
        Ty::I16 => v as u16 as u64,
        Ty::I32 => v as u32 as u64,
        _ => v as u64,
    }
}

#[derive(Clone, Copy)]
enum K {
    I(i64),
    F(f64),
}

// --------------------------------------------------------- constant folding
fn fold_constants(f: &mut Function) -> bool {
    let nparams = f.params.len();
    let mut defs = vec![0u32; f.vregs.len()];
    for d in defs.iter_mut().take(nparams) {
        *d += 1;
    }
    for b in &f.blocks {
        for i in &b.insts {
            if let Some(d) = def_of(i) {
                defs[d as usize] += 1;
            }
        }
    }
    // single-definition constants and copies
    let mut consts: HashMap<VReg, K> = HashMap::new();
    let mut copies: HashMap<VReg, VReg> = HashMap::new();
    for b in &f.blocks {
        for i in &b.insts {
            match i {
                Inst::IConst { dst, val } if defs[*dst as usize] == 1 => {
                    consts.insert(*dst, K::I(canon(f.vregs[*dst as usize], *val)));
                }
                Inst::FConst { dst, val } if defs[*dst as usize] == 1 => {
                    consts.insert(*dst, K::F(*val));
                }
                Inst::Copy { dst, src } if defs[*dst as usize] == 1 && defs[*src as usize] == 1 => {
                    copies.insert(*dst, *src);
                }
                _ => {}
            }
        }
    }
    let resolve = |mut v: VReg| {
        let mut n = 0;
        while let Some(s) = copies.get(&v) {
            v = *s;
            n += 1;
            if n > 64 {
                break;
            }
        }
        v
    };
    let mut changed = false;
    let vregs = f.vregs.clone();
    for b in &mut f.blocks {
        for i in &mut b.insts {
            // copy propagation
            let mut local_change = false;
            for_uses(i, |u| {
                let r = resolve(*u);
                if r != *u {
                    *u = r;
                    local_change = true;
                }
            });
            changed |= local_change;
            let get = |v: VReg| consts.get(&v).copied();
            let replacement: Option<Inst> = match i {
                Inst::Copy { dst, src } => match get(*src) {
                    Some(K::I(v)) => Some(Inst::IConst { dst: *dst, val: v }),
                    Some(K::F(v)) => Some(Inst::FConst { dst: *dst, val: v }),
                    None => None,
                },
                Inst::Bin { dst, op, a, b: bb } => {
                    let ty = vregs[*a as usize];
                    match (get(*a), get(*bb)) {
                        (Some(K::I(x)), Some(K::I(y))) => fold_int_bin(*op, ty, x, y).map(|v| Inst::IConst { dst: *dst, val: v }),
                        (Some(K::F(x)), Some(K::F(y))) => fold_float_bin(*op, x, y).map(|v| Inst::FConst { dst: *dst, val: v }),
                        (_, Some(K::I(y))) => identity_rhs(*op, y).then_some(Inst::Copy { dst: *dst, src: *a }),
                        (Some(K::I(x)), _) => identity_lhs(*op, x).then_some(Inst::Copy { dst: *dst, src: *bb }),
                        _ => None,
                    }
                }
                Inst::Cmp { dst, op, a, b: bb } => {
                    let ty = vregs[*a as usize];
                    match (get(*a), get(*bb)) {
                        (Some(K::I(x)), Some(K::I(y))) => Some(Inst::IConst { dst: *dst, val: fold_icmp(*op, ty, x, y) as i64 }),
                        (Some(K::F(x)), Some(K::F(y))) => Some(Inst::IConst { dst: *dst, val: fold_fcmp(*op, x, y) as i64 }),
                        _ => None,
                    }
                }
                Inst::Un { dst, op, a } => {
                    let ty = vregs[*a as usize];
                    match (get(*a), op) {
                        (Some(K::I(x)), UnOp::Neg) => Some(Inst::IConst { dst: *dst, val: canon(ty, x.wrapping_neg()) }),
                        (Some(K::I(x)), UnOp::Not) => Some(Inst::IConst { dst: *dst, val: canon(ty, !x) }),
                        (Some(K::F(x)), UnOp::FNeg) => Some(Inst::FConst { dst: *dst, val: -x }),
                        (Some(K::F(x)), UnOp::FSqrt) => Some(Inst::FConst { dst: *dst, val: x.sqrt() }),
                        (Some(K::F(x)), UnOp::FAbs) => Some(Inst::FConst { dst: *dst, val: x.abs() }),
                        (Some(K::F(x)), UnOp::FFloor) => Some(Inst::FConst { dst: *dst, val: x.floor() }),
                        (Some(K::F(x)), UnOp::FCeil) => Some(Inst::FConst { dst: *dst, val: x.ceil() }),
                        _ => None,
                    }
                }
                Inst::Cast { dst, op, src } => {
                    let from = vregs[*src as usize];
                    let to = vregs[*dst as usize];
                    match (get(*src), op) {
                        (Some(K::I(x)), CastOp::ZExt) => Some(Inst::IConst { dst: *dst, val: canon(to, ubits(from, x) as i64) }),
                        (Some(K::I(x)), CastOp::SExt) => Some(Inst::IConst { dst: *dst, val: x }),
                        (Some(K::I(x)), CastOp::Trunc) => Some(Inst::IConst { dst: *dst, val: canon(to, x) }),
                        (Some(K::I(x)), CastOp::SToF) => Some(Inst::FConst { dst: *dst, val: x as f64 }),
                        (Some(K::I(x)), CastOp::UToF) => Some(Inst::FConst { dst: *dst, val: ubits(from, x) as f64 }),
                        (Some(K::F(x)), CastOp::FToS) => Some(Inst::IConst { dst: *dst, val: x as i64 }),
                        (Some(K::F(x)), CastOp::FToU) => Some(Inst::IConst { dst: *dst, val: x as u64 as i64 }),
                        _ => None,
                    }
                }
                Inst::Select { dst, cond, a, b: bb } => match get(*cond) {
                    Some(K::I(c)) => Some(Inst::Copy { dst: *dst, src: if c & 1 != 0 { *a } else { *bb } }),
                    _ => None,
                },
                _ => None,
            };
            if let Some(r) = replacement {
                *i = r;
                changed = true;
            }
        }
        term_uses(&mut b.term, |u| {
            let r = resolve(*u);
            if r != *u {
                *u = r;
                changed = true;
            }
        });
        if let Term::Br { cond, then_, else_ } = &b.term {
            if let Some(K::I(c)) = consts.get(cond) {
                b.term = Term::Jump(if c & 1 != 0 { *then_ } else { *else_ });
                changed = true;
            } else if then_ == else_ {
                b.term = Term::Jump(*then_);
                changed = true;
            }
        }
    }
    changed
}

fn identity_rhs(op: BinOp, y: i64) -> bool {
    matches!(
        (op, y),
        (BinOp::Add | BinOp::Sub | BinOp::Or | BinOp::Xor | BinOp::Shl | BinOp::SShr | BinOp::UShr, 0)
            | (BinOp::Mul | BinOp::SDiv | BinOp::UDiv, 1)
    )
}
fn identity_lhs(op: BinOp, x: i64) -> bool {
    matches!((op, x), (BinOp::Add | BinOp::Or | BinOp::Xor, 0) | (BinOp::Mul, 1))
}

fn fold_int_bin(op: BinOp, ty: Ty, x: i64, y: i64) -> Option<i64> {
    let r = match op {
        BinOp::Add => x.wrapping_add(y),
        BinOp::Sub => x.wrapping_sub(y),
        BinOp::Mul => x.wrapping_mul(y),
        BinOp::SDiv => {
            if y == 0 || (y == -1 && x == i64::MIN) {
                return None;
            }
            x / y
        }
        BinOp::SRem => {
            if y == 0 || y == -1 {
                return None;
            }
            x % y
        }
        BinOp::UDiv => {
            let (a, b) = (ubits(ty, x), ubits(ty, y));
            if b == 0 {
                return None;
            }
            (a / b) as i64
        }
        BinOp::URem => {
            let (a, b) = (ubits(ty, x), ubits(ty, y));
            if b == 0 {
                return None;
            }
            (a % b) as i64
        }
        BinOp::And => x & y,
        BinOp::Or => x | y,
        BinOp::Xor => x ^ y,
        BinOp::Shl => x.wrapping_shl((y as u32) & (ty.bytes() * 8 - 1)),
        BinOp::SShr => x >> ((y as u32) & (ty.bytes() * 8 - 1)),
        BinOp::UShr => (ubits(ty, x) >> ((y as u32) & (ty.bytes() * 8 - 1))) as i64,
        _ => return None,
    };
    Some(canon(ty, r))
}

fn fold_float_bin(op: BinOp, x: f64, y: f64) -> Option<f64> {
    Some(match op {
        BinOp::FAdd => x + y,
        BinOp::FSub => x - y,
        BinOp::FMul => x * y,
        BinOp::FDiv => x / y,
        _ => return None,
    })
}

fn fold_icmp(op: CmpOp, ty: Ty, x: i64, y: i64) -> bool {
    let (ux, uy) = (ubits(ty, x), ubits(ty, y));
    match op {
        CmpOp::Eq => x == y,
        CmpOp::Ne => x != y,
        CmpOp::SLt => x < y,
        CmpOp::SLe => x <= y,
        CmpOp::SGt => x > y,
        CmpOp::SGe => x >= y,
        CmpOp::ULt => ux < uy,
        CmpOp::ULe => ux <= uy,
        CmpOp::UGt => ux > uy,
        CmpOp::UGe => ux >= uy,
        _ => false,
    }
}

fn fold_fcmp(op: CmpOp, x: f64, y: f64) -> bool {
    match op {
        CmpOp::FEq => x == y,
        CmpOp::FNe => x != y,
        CmpOp::FLt => x < y,
        CmpOp::FLe => x <= y,
        CmpOp::FGt => x > y,
        CmpOp::FGe => x >= y,
        _ => false,
    }
}

// ------------------------------------------------------------ CFG cleanup
fn successors(t: &Term) -> Vec<BlockId> {
    match t {
        Term::Jump(b) => vec![*b],
        Term::Br { then_, else_, .. } => vec![*then_, *else_],
        _ => vec![],
    }
}

fn simplify_cfg(f: &mut Function) -> bool {
    let mut changed = false;
    // 1. thread jumps through empty blocks
    let n = f.blocks.len();
    let mut target: Vec<BlockId> = (0..n as u32).collect();
    for i in 0..n {
        if f.blocks[i].insts.is_empty() {
            if let Term::Jump(t) = f.blocks[i].term {
                if t as usize != i {
                    target[i] = t;
                }
            }
        }
    }
    let resolve = |mut b: BlockId| {
        for _ in 0..16 {
            let t = target[b as usize];
            if t == b {
                break;
            }
            b = t;
        }
        b
    };
    for b in &mut f.blocks {
        match &mut b.term {
            Term::Jump(t) => {
                let r = resolve(*t);
                if r != *t {
                    *t = r;
                    changed = true;
                }
            }
            Term::Br { then_, else_, .. } => {
                for t in [then_, else_] {
                    let r = resolve(*t);
                    if r != *t {
                        *t = r;
                        changed = true;
                    }
                }
            }
            _ => {}
        }
    }
    // 2. merge a block into its unique predecessor
    loop {
        let mut preds = vec![0u32; f.blocks.len()];
        let mut reachable = vec![false; f.blocks.len()];
        reachable[0] = true;
        let mut stack = vec![0usize];
        while let Some(b) = stack.pop() {
            for s in successors(&f.blocks[b].term) {
                preds[s as usize] += 1;
                if !reachable[s as usize] {
                    reachable[s as usize] = true;
                    stack.push(s as usize);
                }
            }
        }
        let mut merged = false;
        for a in 0..f.blocks.len() {
            if !reachable[a] {
                continue;
            }
            if let Term::Jump(b) = f.blocks[a].term {
                let b = b as usize;
                if b != a && b != 0 && preds[b] == 1 {
                    let blk = std::mem::replace(&mut f.blocks[b], Block { insts: vec![], term: Term::Unreachable });
                    f.blocks[a].insts.extend(blk.insts);
                    f.blocks[a].term = blk.term;
                    merged = true;
                    changed = true;
                    break;
                }
            }
        }
        if !merged {
            break;
        }
    }
    // 3. drop unreachable blocks and renumber
    let mut reachable = vec![false; f.blocks.len()];
    reachable[0] = true;
    let mut stack = vec![0usize];
    while let Some(b) = stack.pop() {
        for s in successors(&f.blocks[b].term) {
            if !reachable[s as usize] {
                reachable[s as usize] = true;
                stack.push(s as usize);
            }
        }
    }
    if reachable.iter().any(|r| !r) {
        let mut remap = vec![u32::MAX; f.blocks.len()];
        let mut next = 0;
        for (i, r) in reachable.iter().enumerate() {
            if *r {
                remap[i] = next;
                next += 1;
            }
        }
        let old = std::mem::take(&mut f.blocks);
        for (i, mut b) in old.into_iter().enumerate() {
            if !reachable[i] {
                continue;
            }
            match &mut b.term {
                Term::Jump(t) => *t = remap[*t as usize],
                Term::Br { then_, else_, .. } => {
                    *then_ = remap[*then_ as usize];
                    *else_ = remap[*else_ as usize];
                }
                _ => {}
            }
            f.blocks.push(b);
        }
        changed = true;
    }
    changed
}

// ------------------------------------------------------- dead-code removal
fn dead_code(f: &mut Function) -> bool {
    let mut changed = false;
    loop {
        let mut uses = vec![0u32; f.vregs.len()];
        for b in &mut f.blocks {
            for i in &mut b.insts {
                for_uses(i, |u| uses[*u as usize] += 1);
            }
            term_uses(&mut b.term, |u| uses[*u as usize] += 1);
        }
        let mut removed = false;
        for b in &mut f.blocks {
            let before = b.insts.len();
            b.insts.retain(|i| match def_of(i) {
                Some(d) if is_pure(i) => uses[d as usize] > 0,
                _ => true,
            });
            if b.insts.len() != before {
                removed = true;
            }
        }
        if !removed {
            break;
        }
        changed = true;
    }
    changed
}

// ---------------------------------------------------------------- inlining
fn inst_count(f: &Function) -> usize {
    f.blocks.iter().map(|b| b.insts.len() + 1).sum()
}

fn calls_func(f: &Function, target: u32) -> bool {
    f.blocks.iter().any(|b| b.insts.iter().any(|i| matches!(i, Inst::Call { callee: Callee::Func(c), .. } if *c == target)))
}

const INLINE_LIMIT: usize = 60;

fn inline_limit() -> usize {
    std::env::var("FLASH_DEBUG_INLINE_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(INLINE_LIMIT)
}

fn inline_small_functions(m: &mut Module) {
    for _round in 0..3 {
        let candidates: Vec<Option<Function>> = m
            .funcs
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let ok = !f.exported && !f.no_inline && inst_count(f) <= inline_limit() && !calls_func(f, i as u32);
                ok.then(|| f.clone())
            })
            .collect();
        let mut any = false;
        for fi in 0..m.funcs.len() {
            let mut f = std::mem::replace(&mut m.funcs[fi], placeholder());
            any |= inline_into(&mut f, fi as u32, &candidates);
            m.funcs[fi] = f;
        }
        if !any {
            break;
        }
    }
}

fn placeholder() -> Function {
    Function {
        name: String::new(),
        params: vec![],
        ret: None,
        vregs: vec![],
        blocks: vec![],
        stack_slots: vec![],
        exported: false,
        no_inline: true,
    }
}

fn inline_into(f: &mut Function, self_idx: u32, cands: &[Option<Function>]) -> bool {
    let mut did = false;
    let mut budget = 200; // bound total growth
    'outer: loop {
        for bi in 0..f.blocks.len() {
            for ii in 0..f.blocks[bi].insts.len() {
                let Inst::Call { dst, callee: Callee::Func(c), args } = f.blocks[bi].insts[ii].clone() else { continue };
                if c == self_idx {
                    continue;
                }
                let Some(callee) = &cands[c as usize] else { continue };
                if budget == 0 {
                    break 'outer;
                }
                budget -= 1;
                splice(f, bi, ii, dst, &args, callee);
                did = true;
                continue 'outer;
            }
        }
        break;
    }
    did
}

/// Replace the call at (bi, ii) with the body of `callee`.
fn splice(f: &mut Function, bi: usize, ii: usize, dst: Option<VReg>, args: &[VReg], callee: &Function) {
    let vbase = f.vregs.len() as u32;
    f.vregs.extend(callee.vregs.iter().copied());
    let sbase = f.stack_slots.len() as u32;
    f.stack_slots.extend(callee.stack_slots.iter().copied());
    let bbase = f.blocks.len() as u32 + 1; // block 0 of the callee goes at index bbase
                                           // continuation block: instructions after the call + original terminator
    let tail: Vec<Inst> = f.blocks[bi].insts.split_off(ii + 1);
    f.blocks[bi].insts.pop(); // the call itself
    let old_term = std::mem::replace(&mut f.blocks[bi].term, Term::Jump(bbase));
    let cont_id = f.blocks.len() as u32;
    f.blocks.push(Block { insts: tail, term: old_term });
    // bind parameters
    for (i, a) in args.iter().enumerate() {
        f.blocks[bi].insts.push(Inst::Copy { dst: vbase + i as u32, src: *a });
    }
    for cb in &callee.blocks {
        let mut insts: Vec<Inst> = vec![];
        for inst in &cb.insts {
            let mut ni = inst.clone();
            remap_inst(&mut ni, vbase, sbase);
            insts.push(ni);
        }
        let term = match &cb.term {
            Term::Jump(t) => Term::Jump(t + bbase),
            Term::Br { cond, then_, else_ } => Term::Br { cond: cond + vbase, then_: then_ + bbase, else_: else_ + bbase },
            Term::Ret(v) => {
                if let (Some(d), Some(v)) = (dst, v) {
                    insts.push(Inst::Copy { dst: d, src: *v + vbase });
                }
                Term::Jump(cont_id)
            }
            Term::Unreachable => Term::Unreachable,
        };
        f.blocks.push(Block { insts, term });
    }
}

fn remap_inst(i: &mut Inst, vbase: u32, sbase: u32) {
    match i {
        Inst::IConst { dst, .. } | Inst::FConst { dst, .. } | Inst::DataAddr { dst, .. } => *dst += vbase,
        Inst::StackAddr { dst, slot } => {
            *dst += vbase;
            *slot += sbase;
        }
        Inst::Copy { dst, src } | Inst::Cast { dst, src, .. } => {
            *dst += vbase;
            *src += vbase;
        }
        Inst::Un { dst, a, .. } => {
            *dst += vbase;
            *a += vbase;
        }
        Inst::Bin { dst, a, b, .. } | Inst::Cmp { dst, a, b, .. } => {
            *dst += vbase;
            *a += vbase;
            *b += vbase;
        }
        Inst::Load { dst, base, .. } => {
            *dst += vbase;
            *base += vbase;
        }
        Inst::Store { base, val, .. } => {
            *base += vbase;
            *val += vbase;
        }
        Inst::Call { dst, callee, args } => {
            if let Some(d) = dst {
                *d += vbase;
            }
            if let Callee::Indirect(v) = callee {
                *v += vbase;
            }
            for a in args {
                *a += vbase;
            }
        }
        Inst::Select { dst, cond, a, b } => {
            *dst += vbase;
            *cond += vbase;
            *a += vbase;
            *b += vbase;
        }
    }
}

// ------------------------------------------------------ unused functions
/// Drop functions that are never called (glue that got fully inlined, unused user functions).
/// Function indices are call targets, so unused functions are replaced by empty stubs would
/// break numbering; instead we renumber.
pub fn remove_unused_functions(m: &mut Module) {
    let n = m.funcs.len();
    let mut live = vec![false; n];
    let mut stack = vec![];
    for (i, f) in m.funcs.iter().enumerate() {
        if f.exported {
            live[i] = true;
            stack.push(i);
        }
    }
    while let Some(i) = stack.pop() {
        for b in &m.funcs[i].blocks {
            for inst in &b.insts {
                if let Inst::Call { callee: Callee::Func(c), .. } = inst {
                    if !live[*c as usize] {
                        live[*c as usize] = true;
                        stack.push(*c as usize);
                    }
                }
            }
        }
    }
    if live.iter().all(|l| *l) {
        return;
    }
    let mut remap = vec![u32::MAX; n];
    let mut next = 0;
    for i in 0..n {
        if live[i] {
            remap[i] = next;
            next += 1;
        }
    }
    let old = std::mem::take(&mut m.funcs);
    for (i, mut f) in old.into_iter().enumerate() {
        if !live[i] {
            continue;
        }
        for b in &mut f.blocks {
            for inst in &mut b.insts {
                if let Inst::Call { callee: Callee::Func(c), .. } = inst {
                    *c = remap[*c as usize];
                }
            }
        }
        m.funcs.push(f);
    }
}

// ------------------------------------------------------ copies and loops
fn uses_vreg(i: &Inst, v: VReg) -> bool {
    let mut found = false;
    let mut c = i.clone();
    for_uses(&mut c, |u| found |= *u == v);
    found
}

fn set_def(i: &mut Inst, nd: VReg) {
    match i {
        Inst::IConst { dst, .. }
        | Inst::FConst { dst, .. }
        | Inst::Copy { dst, .. }
        | Inst::Bin { dst, .. }
        | Inst::Cmp { dst, .. }
        | Inst::Un { dst, .. }
        | Inst::Cast { dst, .. }
        | Inst::Load { dst, .. }
        | Inst::DataAddr { dst, .. }
        | Inst::StackAddr { dst, .. }
        | Inst::Select { dst, .. } => *dst = nd,
        Inst::Call { dst, .. } => *dst = Some(nd),
        Inst::Store { .. } => {}
    }
}

/// Within a basic block, replace uses of `d` by `s` after `d = s` while neither is reassigned.
fn local_copy_propagation(f: &mut Function) -> bool {
    let mut changed = false;
    let vregs = &f.vregs;
    for b in &mut f.blocks {
        let mut map: HashMap<VReg, VReg> = HashMap::new();
        for inst in &mut b.insts {
            for_uses(inst, |u| {
                if let Some(s) = map.get(u) {
                    *u = *s;
                    changed = true;
                }
            });
            if let Some(d) = def_of(inst) {
                map.retain(|k, v| *k != d && *v != d);
            }
            if let Inst::Copy { dst, src } = inst {
                if dst != src && vregs[*dst as usize] == vregs[*src as usize] {
                    map.insert(*dst, *src);
                }
            }
        }
        term_uses(&mut b.term, |u| {
            if let Some(s) = map.get(u) {
                *u = *s;
                changed = true;
            }
        });
    }
    changed
}

/// `t = a op b; ...; d = t` with `t` used only by the copy: compute straight into `d`.
fn fold_copies_into_defs(f: &mut Function) -> bool {
    let nparams = f.params.len();
    let mut defs = vec![0u32; f.vregs.len()];
    let mut uses = vec![0u32; f.vregs.len()];
    for b in &mut f.blocks {
        for i in &mut b.insts {
            if let Some(d) = def_of(i) {
                defs[d as usize] += 1;
            }
            for_uses(i, |u| uses[*u as usize] += 1);
        }
        term_uses(&mut b.term, |u| uses[*u as usize] += 1);
    }
    let mut changed = false;
    for bi in 0..f.blocks.len() {
        let mut k = 0;
        while k < f.blocks[bi].insts.len() {
            let Inst::Copy { dst: d, src: s } = f.blocks[bi].insts[k] else {
                k += 1;
                continue;
            };
            if d != s && (s as usize) >= nparams && defs[s as usize] == 1 && uses[s as usize] == 1 && f.vregs[d as usize] == f.vregs[s as usize] {
                let insts = &f.blocks[bi].insts;
                if let Some(j) = (0..k).rev().find(|j| def_of(&insts[*j]) == Some(s)) {
                    let clear = (j + 1..k).all(|m| !uses_vreg(&insts[m], d) && def_of(&insts[m]) != Some(d));
                    if clear {
                        set_def(&mut f.blocks[bi].insts[j], d);
                        f.blocks[bi].insts.remove(k);
                        defs[d as usize] = defs[d as usize].max(1);
                        changed = true;
                        continue;
                    }
                }
            }
            k += 1;
        }
    }
    changed
}

/// Loop rotation by tail duplication: a jump to a small block that only tests a condition and
/// branches is replaced by a copy of that test, so a loop runs with one conditional branch per
/// iteration instead of a test at the top and an unconditional jump at the bottom.
fn rotate_loops(f: &mut Function) -> bool {
    let n = f.blocks.len();
    let mut preds = vec![0u32; n];
    for b in &f.blocks {
        match &b.term {
            Term::Jump(t) => preds[*t as usize] += 1,
            Term::Br { then_, else_, .. } => {
                preds[*then_ as usize] += 1;
                preds[*else_ as usize] += 1;
            }
            _ => {}
        }
    }
    let mut changed = false;
    for bi in 0..n {
        let Term::Jump(h) = f.blocks[bi].term else { continue };
        let h = h as usize;
        if h == bi || preds[h] < 2 {
            continue;
        }
        let hb = &f.blocks[h];
        if matches!(hb.term, Term::Br { .. }) && hb.insts.len() <= 3 && !hb.insts.iter().any(|i| matches!(i, Inst::Call { .. })) {
            let (insts, term) = (hb.insts.clone(), hb.term.clone());
            f.blocks[bi].insts.extend(insts);
            f.blocks[bi].term = term;
            changed = true;
        }
    }
    changed
}

/// Jump threading for boolean conditions copied into other registers: if block B branches on `c`
/// and an edge leads to an empty block that branches on a copy of `c`, the outcome of that second
/// branch is known on that edge, so the edge goes straight to the right successor. This turns the
/// materialised Bool of a short-circuit `a and b` back into plain control flow.
fn thread_known_conditions(f: &mut Function) -> bool {
    let mut changed = false;
    for bi in 0..f.blocks.len() {
        let Term::Br { cond: c, then_, else_ } = f.blocks[bi].term else { continue };
        // Registers that hold the same value as `c` at the end of the block.
        let mut equal: Vec<VReg> = vec![c];
        let insts = &f.blocks[bi].insts;
        for (j, i) in insts.iter().enumerate() {
            if let Inst::Copy { dst, src } = i {
                if *src == c && *dst != c {
                    let later_redefined = insts[j + 1..].iter().any(|m| matches!(def_of(m), Some(d) if d == *dst || d == c));
                    if !later_redefined {
                        equal.push(*dst);
                    }
                }
            }
        }
        let mut new_then = then_;
        let mut new_else = else_;
        let target_of = |blocks: &[Block], t: u32, taken: bool| -> Option<u32> {
            let tb = &blocks[t as usize];
            if !tb.insts.is_empty() {
                return None;
            }
            match tb.term {
                Term::Br { cond, then_: x, else_: y } if equal.contains(&cond) => Some(if taken { x } else { y }),
                _ => None,
            }
        };
        if let Some(x) = target_of(&f.blocks, then_, true) {
            new_then = x;
        }
        if let Some(y) = target_of(&f.blocks, else_, false) {
            new_else = y;
        }
        if (new_then, new_else) != (then_, else_) {
            f.blocks[bi].term = Term::Br { cond: c, then_: new_then, else_: new_else };
            changed = true;
        }
    }
    changed
}

/// Remove pure definitions whose value is never read before being overwritten (liveness-based,
/// so it also removes dead assignments to registers that are used elsewhere).
fn dead_defs(f: &mut Function) -> bool {
    let nv = f.vregs.len();
    let nb = f.blocks.len();
    let words = nv.div_ceil(64);
    let succs = |t: &Term| -> Vec<usize> {
        match t {
            Term::Jump(b) => vec![*b as usize],
            Term::Br { then_, else_, .. } => vec![*then_ as usize, *else_ as usize],
            _ => vec![],
        }
    };
    // use/def sets per block
    let mut use_b = vec![vec![0u64; words]; nb];
    let mut def_b = vec![vec![0u64; words]; nb];
    let set = |v: &mut Vec<u64>, i: usize| v[i / 64] |= 1 << (i % 64);
    let get = |v: &Vec<u64>, i: usize| v[i / 64] >> (i % 64) & 1 != 0;
    for (bi, b) in f.blocks.iter_mut().enumerate() {
        for i in &mut b.insts {
            for_uses(i, |u| {
                if !get(&def_b[bi], *u as usize) {
                    set(&mut use_b[bi], *u as usize);
                }
            });
            if let Some(d) = def_of(i) {
                set(&mut def_b[bi], d as usize);
            }
        }
        term_uses(&mut b.term, |u| {
            if !get(&def_b[bi], *u as usize) {
                set(&mut use_b[bi], *u as usize);
            }
        });
    }
    let mut live_in = use_b.clone();
    let mut live_out = vec![vec![0u64; words]; nb];
    loop {
        let mut changed = false;
        for bi in (0..nb).rev() {
            let mut out = vec![0u64; words];
            for s in succs(&f.blocks[bi].term) {
                for (o, l) in out.iter_mut().zip(&live_in[s]) {
                    *o |= *l;
                }
            }
            for w in 0..words {
                let inn = use_b[bi][w] | (out[w] & !def_b[bi][w]);
                if inn != live_in[bi][w] {
                    live_in[bi][w] = inn;
                    changed = true;
                }
            }
            live_out[bi] = out;
        }
        if !changed {
            break;
        }
    }
    let mut changed = false;
    for bi in 0..nb {
        let mut live = live_out[bi].clone();
        let blk = &mut f.blocks[bi];
        term_uses(&mut blk.term, |u| set(&mut live, *u as usize));
        let mut keep = vec![true; blk.insts.len()];
        for k in (0..blk.insts.len()).rev() {
            let inst = &mut blk.insts[k];
            if let Some(d) = def_of(inst) {
                if !get(&live, d as usize) && is_pure(inst) {
                    keep[k] = false;
                    changed = true;
                    continue;
                }
                live[d as usize / 64] &= !(1 << (d as usize % 64));
            }
            for_uses(inst, |u| set(&mut live, *u as usize));
        }
        if keep.iter().any(|k| !*k) {
            let mut it = keep.iter();
            blk.insts.retain(|_| *it.next().unwrap());
        }
    }
    changed
}
