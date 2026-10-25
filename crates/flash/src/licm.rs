//! Loop-invariant code motion.
//!
//! For every natural loop (innermost first) a preheader is created or reused, and instructions
//! whose operands do not change inside the loop are moved there:
//! * arithmetic, comparisons, casts and address computations (never divisions, which can trap);
//! * loads with a constant offset from an invariant base, when nothing in the loop can write
//!   their alias class (no call, no store of the same tag or of an unknown tag) and the load sits
//!   in a block that every loop exit passes through, so that it would have run anyway.
//! The IR is not SSA, so a moved instruction must be the only definition of its register.

use crate::cfg::{self, dominates, Cfg};
use crate::ir::*;

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

fn uses(i: &Inst) -> Vec<VReg> {
    match i {
        Inst::IConst { .. } | Inst::FConst { .. } | Inst::DataAddr { .. } | Inst::StackAddr { .. } => vec![],
        Inst::Copy { src, .. } | Inst::Cast { src, .. } => vec![*src],
        Inst::Un { a, .. } => vec![*a],
        Inst::Bin { a, b, .. } | Inst::Cmp { a, b, .. } => vec![*a, *b],
        Inst::Load { base, .. } => vec![*base],
        Inst::Store { base, val, .. } => vec![*base, *val],
        Inst::Call { callee, args, .. } => {
            let mut v = args.clone();
            if let Callee::Indirect(r) = callee {
                v.push(*r);
            }
            v
        }
        Inst::Select { cond, a, b, .. } => vec![*cond, *a, *b],
    }
}

struct Loop {
    header: usize,
    body: Vec<bool>,
}

fn find_loops(f: &Function, cfg: &Cfg) -> Vec<Loop> {
    let n = f.blocks.len();
    let mut loops: Vec<Loop> = vec![];
    for &u in &cfg.rpo {
        for &h in &cfg.succs[u] {
            if cfg.rpo_index[h] != usize::MAX && dominates(cfg, h, u) {
                // back edge u -> h
                let mut body = vec![false; n];
                body[h] = true;
                let mut work = vec![];
                if !body[u] {
                    body[u] = true;
                    work.push(u);
                }
                while let Some(x) = work.pop() {
                    for &p in &cfg.preds[x] {
                        if !body[p] {
                            body[p] = true;
                            work.push(p);
                        }
                    }
                }
                match loops.iter_mut().find(|l| l.header == h) {
                    Some(l) => {
                        for (a, b) in l.body.iter_mut().zip(&body) {
                            *a |= *b;
                        }
                    }
                    None => loops.push(Loop { header: h, body }),
                }
            }
        }
    }
    loops.sort_by_key(|l| l.body.iter().filter(|b| **b).count());
    loops
}

/// Returns a block that runs exactly once before the loop is entered and ends in a jump to the header.
fn preheader(f: &mut Function, cfg: &Cfg, l: &Loop) -> Option<usize> {
    let h = l.header;
    let outside: Vec<usize> = cfg.preds[h].iter().copied().filter(|p| !l.body[*p]).collect();
    if outside.is_empty() {
        return None;
    }
    if outside.len() == 1 && matches!(f.blocks[outside[0]].term, Term::Jump(t) if t as usize == h) {
        return Some(outside[0]);
    }
    let p = f.blocks.len();
    f.blocks.push(Block { insts: vec![], term: Term::Jump(h as u32) });
    for o in outside {
        match &mut f.blocks[o].term {
            Term::Jump(t) => {
                if *t as usize == h {
                    *t = p as u32;
                }
            }
            Term::Br { then_, else_, .. } => {
                if *then_ as usize == h {
                    *then_ = p as u32;
                }
                if *else_ as usize == h {
                    *else_ = p as u32;
                }
            }
            _ => {}
        }
    }
    Some(p)
}

pub fn licm(f: &mut Function) -> bool {
    let mut changed = false;
    let mut done_headers: Vec<usize> = vec![];
    loop {
        let cfg = cfg::build(f);
        let loops = find_loops(f, &cfg);
        // Next loop (innermost first) that has not been processed.
        let Some(l) = loops.into_iter().find(|l| !done_headers.contains(&l.header)) else { break };
        done_headers.push(l.header);
        changed |= hoist_loop(f, &cfg, &l);
    }
    changed
}

fn hoist_loop(f: &mut Function, cfg: &Cfg, l: &Loop) -> bool {
    let nv = f.vregs.len();
    // Global definition counts (parameters count as one definition).
    let mut defs_total = vec![0u32; nv];
    for d in defs_total.iter_mut().take(f.params.len()) {
        *d += 1;
    }
    for b in &f.blocks {
        for i in &b.insts {
            if let Some(d) = def_of(i) {
                defs_total[d as usize] += 1;
            }
        }
    }
    // What the loop writes.
    let mut defs_in_loop = vec![0u32; nv];
    let mut has_call = false;
    let mut writes_untagged = false;
    let mut tags_written: Vec<MemTag> = vec![];
    let members: Vec<usize> = cfg.rpo.iter().copied().filter(|b| l.body[*b]).collect();
    for &b in &members {
        for i in &f.blocks[b].insts {
            if let Some(d) = def_of(i) {
                defs_in_loop[d as usize] += 1;
            }
            match i {
                Inst::Call { .. } => has_call = true,
                Inst::Store { tag, .. } => {
                    if *tag == NO_TAG {
                        writes_untagged = true;
                    } else if !tags_written.contains(tag) {
                        tags_written.push(*tag);
                    }
                }
                _ => {}
            }
        }
    }
    // Blocks through which every exit of the loop passes (they dominate each exiting block).
    let exiting: Vec<usize> = members.iter().copied().filter(|b| cfg.succs[*b].iter().any(|s| !l.body[*s])).collect();
    let runs_before_exit = |b: usize| exiting.iter().all(|e| dominates(cfg, b, *e));

    let mut hoisted: Vec<(usize, usize)> = vec![]; // (block, index) in discovery order
    let mut moved = vec![false; 0];
    let _ = &mut moved;
    let mut is_hoisted: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    loop {
        let mut progress = false;
        for &b in &members {
            for (k, inst) in f.blocks[b].insts.iter().enumerate() {
                if is_hoisted.contains(&(b, k)) {
                    continue;
                }
                let Some(d) = def_of(inst) else { continue };
                if defs_total[d as usize] != 1 {
                    continue;
                }
                let invariant_operands = uses(inst).iter().all(|u| defs_in_loop[*u as usize] == 0);
                if !invariant_operands {
                    continue;
                }
                let ok = match inst {
                    Inst::Bin { op, .. } => !matches!(op, BinOp::SDiv | BinOp::UDiv | BinOp::SRem | BinOp::URem),
                    Inst::Cmp { .. } | Inst::Un { .. } | Inst::Cast { .. } | Inst::Select { .. } | Inst::DataAddr { .. } | Inst::StackAddr { .. } => true,
                    Inst::Load { tag, off, .. } => {
                        let clobbered = has_call || writes_untagged || (*tag == NO_TAG && !tags_written.is_empty()) || tags_written.contains(tag);
                        !clobbered && (0..4096).contains(off) && runs_before_exit(b)
                    }
                    // constants are rematerialised for free; copies gain nothing
                    _ => false,
                };
                if ok {
                    is_hoisted.insert((b, k));
                    hoisted.push((b, k));
                    defs_in_loop[d as usize] -= 1;
                    progress = true;
                }
            }
        }
        if !progress {
            break;
        }
    }
    if hoisted.is_empty() {
        return false;
    }
    let Some(p) = preheader(f, cfg, l) else { return false };
    // Move the instructions (in discovery order) and delete them from the loop.
    let moved: Vec<Inst> = hoisted.iter().map(|(b, k)| f.blocks[*b].insts[*k].clone()).collect();
    for &b in &members {
        let mut k = 0;
        f.blocks[b].insts.retain(|_| {
            let keep = !is_hoisted.contains(&(b, k));
            k += 1;
            keep
        });
    }
    f.blocks[p].insts.extend(moved);
    true
}
