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
        | Inst::LoadIdx { dst, .. }
        | Inst::DataAddr { dst, .. }
        | Inst::StackAddr { dst, .. }
        | Inst::Select { dst, .. } => Some(*dst),
        Inst::Call { dst, .. } => *dst,
        Inst::Store { .. } | Inst::StoreIdx { .. } => None,
    }
}

fn uses(i: &Inst) -> Vec<VReg> {
    match i {
        Inst::IConst { .. } | Inst::FConst { .. } | Inst::DataAddr { .. } | Inst::StackAddr { .. } => vec![],
        Inst::Copy { src, .. } | Inst::Cast { src, .. } => vec![*src],
        Inst::Un { a, .. } => vec![*a],
        Inst::Bin { a, b, .. } | Inst::Cmp { a, b, .. } => vec![*a, *b],
        Inst::Load { base, .. } => vec![*base],
        Inst::LoadIdx { base, index, .. } => vec![*base, *index],
        Inst::Store { base, val, .. } => vec![*base, *val],
        Inst::StoreIdx { base, index, val, .. } => vec![*base, *index, *val],
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

struct Summary {
    defs_total: Vec<u32>,
    defs_in_loop: Vec<u32>,
    has_call: bool,
    writes_untagged: bool,
    tags_written: Vec<MemTag>,
}

impl Summary {
    fn clobbers(&self, tag: MemTag) -> bool {
        self.has_call || self.writes_untagged || (tag == NO_TAG && !self.tags_written.is_empty()) || self.tags_written.contains(&tag)
    }
}

fn summarize(f: &Function, cfg: &Cfg, l: &Loop) -> (Summary, Vec<usize>) {
    let nv = f.vregs.len();
    let mut s = Summary { defs_total: vec![0; nv], defs_in_loop: vec![0; nv], has_call: false, writes_untagged: false, tags_written: vec![] };
    for d in s.defs_total.iter_mut().take(f.params.len()) {
        *d += 1;
    }
    for b in &f.blocks {
        for i in &b.insts {
            if let Some(d) = def_of(i) {
                s.defs_total[d as usize] += 1;
            }
        }
    }
    let members: Vec<usize> = cfg.rpo.iter().copied().filter(|b| l.body[*b]).collect();
    for &b in &members {
        for i in &f.blocks[b].insts {
            if let Some(d) = def_of(i) {
                s.defs_in_loop[d as usize] += 1;
            }
            match i {
                Inst::Call { .. } => s.has_call = true,
                Inst::Store { tag, .. } => {
                    if *tag == NO_TAG {
                        s.writes_untagged = true;
                    } else if !s.tags_written.contains(tag) {
                        s.tags_written.push(*tag);
                    }
                }
                _ => {}
            }
        }
    }
    (s, members)
}

pub fn licm(f: &mut Function) -> bool {
    if std::env::var_os("FLASH_DEBUG_TIMES").is_some() {
        let cfg = cfg::build(f);
        eprintln!("    licm {}: {} blocks, {} vregs, {} loops", f.name, f.blocks.len(), f.vregs.len(), find_loops(f, &cfg).len());
    }
    let mut changed = false;
    let mut done_headers: Vec<usize> = vec![];
    loop {
        let cfg = cfg::build(f);
        let loops = find_loops(f, &cfg);
        // Next loop (innermost first) that has not been processed.
        let Some(l) = loops.into_iter().find(|l| !done_headers.contains(&l.header)) else { break };
        done_headers.push(l.header);
        let Some(p) = preheader(f, &cfg, &l) else { continue };
        changed = true;
        // Peel the invariant prefix of the loop body, then hoist from the loop that remains.
        let (cfg, l) = match peel_invariant_prefix(f, &cfg, &l) {
            Some(new_header) => {
                done_headers.push(new_header);
                let cfg2 = cfg::build(f);
                match find_loops(f, &cfg2).into_iter().find(|x| x.header == new_header) {
                    Some(l2) => (cfg2, l2),
                    None => continue,
                }
            }
            None => {
                let cfg2 = cfg::build(f);
                match find_loops(f, &cfg2).into_iter().find(|x| x.header == l.header) {
                    Some(l2) => (cfg2, l2),
                    None => continue,
                }
            }
        };
        let _ = p;
        hoist_loop(f, &cfg, &l);
    }
    changed
}

/// Peel the leading blocks of the loop body whose work is the same on every iteration (pure
/// arithmetic, loads of memory the loop never writes, branches on invariant conditions to code
/// that leaves the loop). They run once, in the preheader's chain, in the original order; the
/// loop then starts at the first block that depends on the iteration. Returns the new header.
fn peel_invariant_prefix(f: &mut Function, cfg: &Cfg, l: &Loop) -> Option<usize> {
    let (sum, _members) = summarize(f, cfg, l);
    let mut peeled_defs: std::collections::HashSet<VReg> = std::collections::HashSet::new();
    let invariant = |u: VReg, peeled: &std::collections::HashSet<VReg>| sum.defs_in_loop[u as usize] == 0 || peeled.contains(&u);
    let mut chain: Vec<usize> = vec![];
    let mut cur = l.header;
    let new_header;
    loop {
        // all instructions must be repeatable-once
        let blk = &f.blocks[cur];
        let mut local: Vec<VReg> = vec![];
        let mut ok = true;
        for inst in &blk.insts {
            let Some(d) = def_of(inst) else {
                ok = false;
                break;
            };
            let kind_ok = match inst {
                Inst::IConst { .. } | Inst::FConst { .. } | Inst::Copy { .. } => true,
                Inst::Bin { op, .. } => !matches!(op, BinOp::SDiv | BinOp::UDiv | BinOp::SRem | BinOp::URem),
                Inst::Cmp { .. } | Inst::Un { .. } | Inst::Cast { .. } | Inst::Select { .. } | Inst::DataAddr { .. } | Inst::StackAddr { .. } => true,
                Inst::Load { tag, off, .. } => !sum.clobbers(*tag) && (0..4096).contains(off),
                _ => false,
            };
            let mut operands_ok = true;
            for u in uses(inst) {
                if !(invariant(u, &peeled_defs) || local.contains(&u)) {
                    operands_ok = false;
                }
            }
            if !kind_ok || !operands_ok || sum.defs_in_loop[d as usize] != 1 {
                ok = false;
                break;
            }
            local.push(d);
        }
        if !ok {
            return if chain.is_empty() { None } else { Some(new_header_for(f, l, &chain, cur)) };
        }
        // terminator form
        let next = match blk.term {
            Term::Jump(t) => Some(t as usize),
            Term::Br { cond, then_, else_ } => {
                let (t, e) = (then_ as usize, else_ as usize);
                let cond_ok = invariant(cond, &peeled_defs) || local.contains(&cond);
                if cond_ok && l.body[t] && !l.body[e] {
                    Some(t)
                } else if cond_ok && l.body[e] && !l.body[t] {
                    Some(e)
                } else {
                    None
                }
            }
            _ => None,
        };
        let Some(next) = next else {
            // The terminator is not peelable, so this block stays in the loop.
            return if chain.is_empty() { None } else { Some(new_header_for(f, l, &chain, cur)) };
        };
        if !l.body[next] || next == l.header {
            return if chain.is_empty() { None } else { Some(new_header_for(f, l, &chain, cur)) };
        }
        chain.push(cur);
        peeled_defs.extend(local);
        if cfg.preds[next].len() == 1 && cfg.preds[next][0] == cur {
            cur = next;
            continue;
        }
        new_header = next;
        break;
    }
    Some(new_header_for(f, l, &chain, new_header))
}

/// The blocks in `chain` leave the loop: edges from inside the loop to the old header now go to
/// `new_header`. Returns `new_header`.
fn new_header_for(f: &mut Function, l: &Loop, chain: &[usize], new_header: usize) -> usize {
    let old = l.header;
    for (b, blk) in f.blocks.iter_mut().enumerate() {
        if !l.body[b] || chain.contains(&b) {
            continue;
        }
        match &mut blk.term {
            Term::Jump(t) => {
                if *t as usize == old {
                    *t = new_header as u32;
                }
            }
            Term::Br { then_, else_, .. } => {
                if *then_ as usize == old {
                    *then_ = new_header as u32;
                }
                if *else_ as usize == old {
                    *else_ = new_header as u32;
                }
            }
            _ => {}
        }
    }
    new_header
}

fn hoist_loop(f: &mut Function, cfg: &Cfg, l: &Loop) -> bool {
    let (sum, members) = summarize(f, cfg, l);
    let Summary { defs_total, mut defs_in_loop, .. } = Summary { defs_total: sum.defs_total.clone(), defs_in_loop: sum.defs_in_loop.clone(), has_call: sum.has_call, writes_untagged: sum.writes_untagged, tags_written: sum.tags_written.clone() };
    // Blocks through which every normal exit of the loop passes (exits into code that ends the
    // program, like panics, do not count: running a harmless load early cannot be observed).
    let ends_program = |b: usize| matches!(f.blocks[b].term, Term::Unreachable);
    let exiting: Vec<usize> = members.iter().copied().filter(|b| cfg.succs[*b].iter().any(|s| !l.body[*s] && !ends_program(*s))).collect();
    let runs_before_exit = |b: usize| exiting.iter().all(|e| dominates(cfg, b, *e));

    let mut hoisted: Vec<(usize, usize)> = vec![];
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
                if !uses(inst).iter().all(|u| defs_in_loop[*u as usize] == 0) {
                    continue;
                }
                let ok = match inst {
                    Inst::Bin { op, .. } => !matches!(op, BinOp::SDiv | BinOp::UDiv | BinOp::SRem | BinOp::URem),
                    Inst::Cmp { .. } | Inst::Un { .. } | Inst::Cast { .. } | Inst::Select { .. } | Inst::DataAddr { .. } | Inst::StackAddr { .. } => true,
                    Inst::Load { tag, off, .. } => !sum.clobbers(*tag) && (0..4096).contains(off) && runs_before_exit(b),
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
