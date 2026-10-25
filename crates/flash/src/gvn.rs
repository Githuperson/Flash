//! Global value numbering over the dominator tree.
//!
//! The IR is not SSA (a virtual register can be assigned several times), so the pass numbers
//! *values*, not registers: every definition produces a value id, and a register maps to the id
//! of the value it currently holds. Expressions with the same operand values are the same value;
//! a redundant computation is replaced by a copy of the register that already holds it.
//!
//! * Facts flow down the dominator tree. At a block with several predecessors, registers assigned
//!   on any path from the immediate dominator and memory classes clobbered on such a path are
//!   forgotten first (this also makes the pass loop-aware: a load before a loop is reused inside
//!   it only if nothing in the loop can change that memory class).
//! * Loads are keyed by (base value, offset, alias tag) and a per-tag "epoch" that stores of the
//!   same tag, calls and untagged stores advance. A store also forwards its value to later loads.
//! * A conditional branch whose condition is known from a dominating branch becomes a jump, which
//!   removes repeated bounds checks.
//! All changes are undone when the traversal leaves a subtree, so the tables stay small.

use std::collections::HashMap;

use crate::cfg::{self, Cfg};
use crate::ir::*;

#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Const(Ty, i64),
    FConst(u64),
    Bin(BinOp, u32, u32),
    Cmp(CmpOp, u32, u32),
    Un(UnOp, u32),
    Cast(CastOp, Ty, u32),
    Select(u32, u32, u32),
    Load(Ty, u32, i32, MemTag, u32, u32),
    Data(DataId),
    Stack(u32),
}

enum Undo {
    Cur(VReg, u32),
    Expr(Key, Option<u32>),
    Rep(u32, Option<VReg>),
    Fact(u32, Option<bool>),
    EpochTag(usize, u32),
    EpochAll(u32),
    EpochAny(u32),
}

struct State {
    cur: Vec<u32>,
    exprs: HashMap<Key, u32>,
    rep: HashMap<u32, VReg>,
    facts: HashMap<u32, bool>,
    epoch_tag: Vec<u32>,
    epoch_all: u32,
    epoch_any: u32,
    next_value: u32,
    journal: Vec<Undo>,
}

impl State {
    fn rollback(&mut self, mark: usize) {
        while self.journal.len() > mark {
            match self.journal.pop().unwrap() {
                Undo::Cur(v, old) => self.cur[v as usize] = old,
                Undo::Expr(k, old) => match old {
                    Some(o) => {
                        self.exprs.insert(k, o);
                    }
                    None => {
                        self.exprs.remove(&k);
                    }
                },
                Undo::Rep(v, old) => match old {
                    Some(o) => {
                        self.rep.insert(v, o);
                    }
                    None => {
                        self.rep.remove(&v);
                    }
                },
                Undo::Fact(v, old) => match old {
                    Some(o) => {
                        self.facts.insert(v, o);
                    }
                    None => {
                        self.facts.remove(&v);
                    }
                },
                Undo::EpochTag(t, old) => self.epoch_tag[t] = old,
                Undo::EpochAll(old) => self.epoch_all = old,
                Undo::EpochAny(old) => self.epoch_any = old,
            }
        }
    }

    fn set_cur(&mut self, v: VReg, val: u32) {
        let old = self.cur[v as usize];
        self.journal.push(Undo::Cur(v, old));
        self.cur[v as usize] = val;
    }

    fn fresh(&mut self) -> u32 {
        self.next_value += 1;
        self.next_value
    }

    /// The value currently held by `v` (a fresh unknown value if it is not tracked).
    fn val(&mut self, v: VReg) -> u32 {
        if self.cur[v as usize] == 0 {
            let id = self.fresh();
            self.set_cur(v, id);
        }
        self.cur[v as usize]
    }

    fn set_expr(&mut self, k: Key, val: u32) {
        let old = self.exprs.insert(k.clone(), val);
        self.journal.push(Undo::Expr(k, old));
    }

    fn set_rep(&mut self, val: u32, v: VReg) {
        let old = self.rep.insert(val, v);
        self.journal.push(Undo::Rep(val, old));
    }

    fn set_fact(&mut self, val: u32, b: bool) {
        let old = self.facts.insert(val, b);
        self.journal.push(Undo::Fact(val, old));
    }

    fn clobber_all(&mut self) {
        self.journal.push(Undo::EpochAll(self.epoch_all));
        self.journal.push(Undo::EpochAny(self.epoch_any));
        self.epoch_all += 1;
        self.epoch_any += 1;
    }

    fn clobber_tag(&mut self, tag: MemTag) {
        if tag == NO_TAG {
            self.clobber_all();
            return;
        }
        let t = tag as usize;
        self.journal.push(Undo::EpochTag(t, self.epoch_tag[t]));
        self.journal.push(Undo::EpochAny(self.epoch_any));
        self.epoch_tag[t] += 1;
        self.epoch_any += 1;
    }

    fn load_key(&self, ty: Ty, base: u32, off: i32, tag: MemTag) -> Key {
        if tag == NO_TAG {
            Key::Load(ty, base, off, tag, self.epoch_any, 0)
        } else {
            Key::Load(ty, base, off, tag, self.epoch_all, self.epoch_tag[tag as usize])
        }
    }
}

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

fn commutative(op: BinOp) -> bool {
    matches!(op, BinOp::Add | BinOp::Mul | BinOp::And | BinOp::Or | BinOp::Xor | BinOp::FAdd | BinOp::FMul)
}

/// What a block may change: registers it assigns and the memory classes it may write.
struct Effects {
    defs: Vec<VReg>,
    all: bool,
    tags: Vec<MemTag>,
}

fn effects_of(b: &Block) -> Effects {
    let mut e = Effects { defs: vec![], all: false, tags: vec![] };
    for i in &b.insts {
        if let Some(d) = def_of(i) {
            e.defs.push(d);
        }
        match i {
            Inst::Call { .. } => e.all = true,
            Inst::Store { tag, .. } => {
                if *tag == NO_TAG {
                    e.all = true;
                } else if !e.tags.contains(tag) {
                    e.tags.push(*tag);
                }
            }
            _ => {}
        }
    }
    e
}

enum Step {
    Enter(usize),
    Exit(usize),
}

pub fn gvn(f: &mut Function, ntags: usize) -> bool {
    let cfg = cfg::build(f);
    if cfg.rpo.is_empty() {
        return false;
    }
    let effects: Vec<Effects> = f.blocks.iter().map(effects_of).collect();
    let mut st = State {
        cur: vec![0; f.vregs.len()],
        exprs: HashMap::new(),
        rep: HashMap::new(),
        facts: HashMap::new(),
        epoch_tag: vec![0; ntags + 1],
        epoch_all: 0,
        epoch_any: 0,
        next_value: 0,
        journal: vec![],
    };
    let mut changed = false;
    // Register types are read-only here; take them out so that blocks can be mutated meanwhile.
    let vregs = std::mem::take(&mut f.vregs);
    let mut stack = vec![Step::Enter(0)];
    while let Some(step) = stack.pop() {
        match step {
            Step::Exit(mark) => st.rollback(mark),
            Step::Enter(b) => {
                let mark = st.journal.len();
                stack.push(Step::Exit(mark));
                enter_block(f, &cfg, &effects, &mut st, b);
                changed |= process_block(f, &vregs, &mut st, b);
                for &c in cfg.children[b].iter().rev() {
                    stack.push(Step::Enter(c));
                }
            }
        }
    }
    f.vregs = vregs;
    changed
}

/// Establish the facts valid on entry to `b` given the state at the end of its dominator.
fn enter_block(f: &Function, cfg: &Cfg, effects: &[Effects], st: &mut State, b: usize) {
    let d = cfg.idom[b];
    if d == usize::MAX {
        return;
    }
    if cfg.preds[b].len() > 1 || cfg.preds[b][0] != d {
        // Everything that happens on a path from the dominator to this block is unknown here.
        let mut region = vec![false; f.blocks.len()];
        let mut work: Vec<usize> = vec![];
        for &p in &cfg.preds[b] {
            if p != d && !region[p] {
                region[p] = true;
                work.push(p);
            }
        }
        while let Some(x) = work.pop() {
            for &p in &cfg.preds[x] {
                if p != d && !region[p] {
                    region[p] = true;
                    work.push(p);
                }
            }
        }
        for (r, inside) in region.iter().enumerate() {
            if !*inside {
                continue;
            }
            for &v in &effects[r].defs {
                if st.cur[v as usize] != 0 {
                    st.set_cur(v, 0);
                }
            }
            if effects[r].all {
                st.clobber_all();
            }
            for &t in &effects[r].tags {
                st.clobber_tag(t);
            }
        }
    } else if let Term::Br { cond, then_, else_ } = f.blocks[d].term {
        // Single predecessor reached over a conditional edge: the condition is known.
        if then_ != else_ {
            let v = st.val(cond);
            st.set_fact(v, b == then_ as usize);
        }
    }
}

fn process_block(f: &mut Function, vregs: &[Ty], st: &mut State, b: usize) -> bool {
    let mut changed = false;
    let n = f.blocks[b].insts.len();
    for k in 0..n {
        let inst = f.blocks[b].insts[k].clone();
        let mut replacement: Option<Inst> = None;
        match &inst {
            Inst::IConst { dst, val } => {
                let key = Key::Const(vregs[*dst as usize], *val);
                numbered_const(st, key, *dst);
            }
            Inst::FConst { dst, val } => {
                numbered_const(st, Key::FConst(val.to_bits()), *dst);
            }
            Inst::Copy { dst, src } => {
                let v = st.val(*src);
                st.set_cur(*dst, v);
            }
            Inst::Bin { dst, op, a, b: bb } => {
                let (mut x, mut y) = (st.val(*a), st.val(*bb));
                if commutative(*op) && x > y {
                    std::mem::swap(&mut x, &mut y);
                }
                replacement = numbered(st, Key::Bin(*op, x, y), *dst);
            }
            Inst::Cmp { dst, op, a, b: bb } => {
                let (mut x, mut y) = (st.val(*a), st.val(*bb));
                if matches!(op, CmpOp::Eq | CmpOp::Ne | CmpOp::FEq | CmpOp::FNe) && x > y {
                    std::mem::swap(&mut x, &mut y);
                }
                replacement = numbered(st, Key::Cmp(*op, x, y), *dst);
            }
            Inst::Un { dst, op, a } => {
                let x = st.val(*a);
                replacement = numbered(st, Key::Un(*op, x), *dst);
            }
            Inst::Cast { dst, op, src } => {
                let x = st.val(*src);
                replacement = numbered(st, Key::Cast(*op, vregs[*dst as usize], x), *dst);
            }
            Inst::Select { dst, cond, a, b: bb } => {
                let (c, x, y) = (st.val(*cond), st.val(*a), st.val(*bb));
                replacement = numbered(st, Key::Select(c, x, y), *dst);
            }
            Inst::DataAddr { dst, data } => {
                numbered_const(st, Key::Data(*data), *dst);
            }
            Inst::StackAddr { dst, slot } => {
                numbered_const(st, Key::Stack(*slot), *dst);
            }
            Inst::Load { dst, base, off, tag } => {
                let bv = st.val(*base);
                let key = st.load_key(vregs[*dst as usize], bv, *off, *tag);
                replacement = numbered(st, key, *dst);
            }
            Inst::Store { base, off, val, tag } => {
                st.clobber_tag(*tag);
                // Forward the stored value to later loads of the same address.
                let bv = st.val(*base);
                let vv = st.val(*val);
                let key = st.load_key(vregs[*val as usize], bv, *off, *tag);
                st.set_expr(key, vv);
                st.set_rep(vv, *val);
            }
            Inst::LoadIdx { .. } | Inst::StoreIdx { .. } => unreachable!("indexed memory instructions are created after GVN"),
            Inst::Call { dst, .. } => {
                st.clobber_all();
                if let Some(d) = dst {
                    let id = st.fresh();
                    st.set_cur(*d, id);
                }
            }
        }
        if let Some(r) = replacement {
            f.blocks[b].insts[k] = r;
            changed = true;
        }
    }
    // A branch on a known condition becomes a jump.
    if let Term::Br { cond, then_, else_ } = f.blocks[b].term {
        let v = st.val(cond);
        if let Some(&known) = st.facts.get(&v) {
            f.blocks[b].term = Term::Jump(if known { then_ } else { else_ });
            changed = true;
        }
    }
    changed
}

/// Number a constant-like value: equal constants share a value id, but the instruction is never
/// replaced (materialising a constant is cheaper than keeping another register alive).
fn numbered_const(st: &mut State, key: Key, dst: VReg) {
    let v = match st.exprs.get(&key) {
        Some(&v) => v,
        None => {
            let v = st.fresh();
            st.set_expr(key, v);
            v
        }
    };
    st.set_cur(dst, v);
}

/// Value-number a computation. Returns a replacement `Copy` if the value is already held by a
/// register.
fn numbered(st: &mut State, key: Key, dst: VReg) -> Option<Inst> {
    if let Some(&v) = st.exprs.get(&key) {
        if let Some(&r) = st.rep.get(&v) {
            if st.cur[r as usize] == v && r != dst {
                st.set_cur(dst, v);
                return Some(Inst::Copy { dst, src: r });
            }
        }
        // The value exists but no register holds it any more: `dst` becomes its holder.
        st.set_cur(dst, v);
        st.set_rep(v, dst);
        return None;
    }
    let v = st.fresh();
    st.set_expr(key, v);
    st.set_rep(v, dst);
    st.set_cur(dst, v);
    None
}
