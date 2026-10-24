//! Ownership checker.
//!
//! Enforces the rules of Flash's restricted ownership model on the typed HIR:
//!
//! 1. A variable of an owned type (String, List, struct, Result) has exactly one owner. Using it
//!    by value *moves* it; using the variable afterwards (on any path, including the next loop
//!    iteration) is an error.
//! 2. While a `for` loop reads a list, the list cannot be modified or moved inside the loop body.
//! 3. A call cannot pass the same owned variable to a `mut`/`own` parameter and to any other
//!    parameter at the same time (no aliasing of exclusive references).
//!
//! The checker that builds the HIR (check.rs) already guarantees that borrows are never stored
//! (borrowed parameters and loop variables cannot be assigned, returned or moved), so a borrow
//! can never outlive its owner. Together these rules rule out use-after-free and double-free in
//! safe Flash code; the tests in tests/ownership.rs exercise them.

use std::collections::HashMap;

use crate::diag::{Diagnostic, Diagnostics};
use crate::hir::*;
use crate::source::Span;

type Moved = HashMap<LocalId, Span>;

struct Ck<'a> {
    f: &'a HFunc,
    prog: &'a Program,
    diags: &'a mut Diagnostics,
    /// `None` means the current point is unreachable (after return/break/continue).
    state: Option<Moved>,
    locked: Vec<(LocalId, Span)>,
    loop_breaks: Vec<Vec<Moved>>,
    loop_continues: Vec<Vec<Moved>>,
}

pub fn check(p: &Program, diags: &mut Diagnostics) {
    for f in &p.funcs {
        let mut ck = Ck {
            f,
            prog: p,
            diags,
            state: Some(HashMap::new()),
            locked: vec![],
            loop_breaks: vec![],
            loop_continues: vec![],
        };
        ck.stmts(&f.body);
    }
}

fn join(a: Option<Moved>, b: Option<Moved>) -> Option<Moved> {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(mut a), Some(b)) => {
            for (k, v) in b {
                a.entry(k).or_insert(v);
            }
            Some(a)
        }
    }
}

fn root(e: &HExpr) -> Option<LocalId> {
    match &e.kind {
        HKind::Local(id, _) => Some(*id),
        HKind::Field(b, _) | HKind::Index(b, _) => root(b),
        _ => None,
    }
}

impl<'a> Ck<'a> {
    fn name(&self, id: LocalId) -> &str {
        &self.f.locals[id].name
    }

    fn stmts(&mut self, ss: &[HStmt]) {
        for s in ss {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &HStmt) {
        match &s.kind {
            HStmtKind::Let(id, e) => {
                self.expr(e);
                if let Some(st) = &mut self.state {
                    st.remove(id);
                }
            }
            HStmtKind::Assign(place, value) => {
                self.expr(value);
                // Evaluating the place reads its base (but not the variable being replaced).
                match &place.kind {
                    HKind::Local(id, _) => {
                        self.check_mutation(*id, place.span);
                        if let Some(st) = &mut self.state {
                            st.remove(id);
                        }
                    }
                    _ => {
                        if let Some(r) = root(place) {
                            self.check_mutation(r, place.span);
                        }
                        self.place_reads(place);
                    }
                }
            }
            HStmtKind::Expr(e) => self.expr(e),
            HStmtKind::If(c, a, b) => {
                self.expr(c);
                let before = self.state.clone();
                self.stmts(a);
                let after_a = std::mem::replace(&mut self.state, before);
                self.stmts(b);
                let after_b = self.state.take();
                self.state = join(after_a, after_b);
            }
            HStmtKind::While(c, body) => {
                self.looped(None, |ck| {
                    ck.expr(c);
                }, body);
            }
            HStmtKind::ForRange { var, lo, hi, body, .. } => {
                self.expr(lo);
                self.expr(hi);
                let v = *var;
                self.looped(None, |ck| {
                    if let Some(st) = &mut ck.state {
                        st.remove(&v);
                    }
                }, body);
            }
            HStmtKind::ForList { var, list, body } => {
                self.expr(list);
                let lock = root(list).map(|r| (r, list.span));
                let v = *var;
                self.looped(lock, |ck| {
                    if let Some(st) = &mut ck.state {
                        st.remove(&v);
                    }
                }, body);
            }
            HStmtKind::Break => {
                if let (Some(st), Some(b)) = (self.state.take(), self.loop_breaks.last_mut()) {
                    b.push(st);
                }
            }
            HStmtKind::Continue => {
                if let (Some(st), Some(b)) = (self.state.take(), self.loop_continues.last_mut()) {
                    b.push(st);
                }
            }
            HStmtKind::Return(e) => {
                if let Some(e) = e {
                    self.expr(e);
                }
                self.state = None;
            }
        }
    }

    /// Analyse a loop to a fixed point so moves in one iteration are seen by the next.
    fn looped(&mut self, lock: Option<(LocalId, Span)>, mut header: impl FnMut(&mut Self), body: &[HStmt]) {
        let entry = self.state.clone();
        let mut loop_entry = entry.clone();
        let mut exit: Option<Moved> = None;
        if let Some(l) = lock {
            self.locked.push(l);
        }
        for _ in 0..3 {
            self.state = loop_entry.clone();
            self.loop_breaks.push(vec![]);
            self.loop_continues.push(vec![]);
            header(self);
            // condition false / range exhausted: leaves the loop with the header's state
            exit = join(exit, self.state.clone());
            self.stmts(body);
            let breaks = self.loop_breaks.pop().unwrap();
            let conts = self.loop_continues.pop().unwrap();
            let mut end = self.state.take();
            for c in conts {
                end = join(end, Some(c));
            }
            for b in breaks {
                exit = join(exit, Some(b));
            }
            let next_entry = join(loop_entry.clone(), end);
            if next_entry == loop_entry {
                break;
            }
            loop_entry = next_entry;
        }
        if lock.is_some() {
            self.locked.pop();
        }
        self.state = exit;
    }

    fn check_mutation(&mut self, root: LocalId, span: Span) {
        if let Some((_, lspan)) = self.locked.iter().find(|(l, _)| *l == root).copied() {
            let n = self.name(root).to_string();
            self.diags.push(
                Diagnostic::error(span, format!("Cannot change or move '{}' while a `for` loop is reading it.", n))
                    .note(lspan, "the loop reads it here")
                    .help("Collect the changes in a separate list and apply them after the loop, or loop over `copy(...)` of it."),
            );
        }
    }

    fn use_local(&mut self, id: LocalId, mode: UseMode, span: Span) {
        let name = self.name(id).to_string();
        let Some(st) = &mut self.state else { return };
        if let Some(moved_at) = st.get(&id).copied() {
            self.diags.push(
                Diagnostic::error(span, format!("The value of '{}' was moved earlier and can't be used again.", name))
                    .note(moved_at, format!("ownership of '{}' moved here", name))
                    .help(format!(
                        "Use `copy({0})` where you moved it to keep '{0}' usable, or pass it to a function parameter without `own` (parameters borrow by default).",
                        name
                    )),
            );
            return;
        }
        if mode == UseMode::Move {
            st.insert(id, span);
            self.check_mutation(id, span);
        }
    }

    fn place_reads(&mut self, e: &HExpr) {
        match &e.kind {
            HKind::Local(id, m) => self.use_local(*id, if *m == UseMode::Move { UseMode::Borrow } else { *m }, e.span),
            HKind::Field(b, _) => self.place_reads(b),
            HKind::Index(b, i) => {
                self.place_reads(b);
                self.expr(i);
            }
            _ => self.expr(e),
        }
    }

    fn expr(&mut self, e: &HExpr) {
        match &e.kind {
            HKind::Int(_) | HKind::Float(_) | HKind::Bool(_) | HKind::Str(_) | HKind::None => {}
            HKind::Local(id, m) => self.use_local(*id, *m, e.span),
            HKind::Unary(_, a) | HKind::Cast(a) | HKind::Try(a) | HKind::Field(a, _) => self.expr(a),
            HKind::Binary(_, a, b) | HKind::Index(a, b) => {
                self.expr(a);
                self.expr(b);
            }
            HKind::Logic(_, a, b) => {
                self.expr(a);
                // the right-hand side may not run
                let before = self.state.clone();
                self.expr(b);
                self.state = join(before, self.state.take());
            }
            HKind::Struct(args) | HKind::List(args) => {
                for a in args {
                    self.expr(a);
                }
            }
            HKind::Builtin(b, args) => {
                if matches!(b, Builtin::Append | Builtin::Pop) {
                    if let Some(r) = args.first().and_then(root) {
                        self.check_mutation(r, e.span);
                    }
                }
                for a in args {
                    self.expr(a);
                }
            }
            HKind::PluginCall { args, .. } | HKind::ExternCall { args, .. } => {
                for a in args {
                    self.expr(a);
                }
            }
            HKind::Call(fid, args) => {
                for a in args {
                    self.expr(a);
                }
                let callee = &self.prog.funcs[*fid];
                // Exclusive access must be unique within the call.
                let mut seen: Vec<(LocalId, bool, Span)> = vec![];
                for (a, p) in args.iter().zip(callee.params.iter()) {
                    if a.ty.is_copy() {
                        continue;
                    }
                    let Some(r) = root(a) else { continue };
                    let exclusive = matches!(callee.locals[*p].kind, LocalKind::ParamMut | LocalKind::ParamOwn);
                    if callee.locals[*p].kind == LocalKind::ParamMut {
                        self.check_mutation(r, a.span);
                    }
                    for (r2, ex2, sp2) in &seen {
                        if *r2 == r && (exclusive || *ex2) {
                            let n = self.f.locals[r].name.clone();
                            self.diags.push(
                                Diagnostic::error(a.span, format!("'{}' is passed more than once to this call, and one use needs exclusive access.", n))
                                    .note(*sp2, "first use")
                                    .help("Pass a `copy(...)` for one of the arguments."),
                            );
                        }
                    }
                    seen.push((r, exclusive, a.span));
                }
            }
        }
    }
}
