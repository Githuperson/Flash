//! Type inference and static checking.
//!
//! Functions without parameter annotations are checked once per distinct tuple of argument
//! types (an *instance*), so `fn add(a, b)` called with `(Int, Int)` and `(Float, Float)` produces
//! two typed functions. Return types are inferred from `return` statements.

use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::*;
use crate::builtins::is_builtin;
use crate::diag::{suggest, Diagnostic, Diagnostics};
use crate::hir::*;
use crate::module::LoadedModule;
use crate::source::Span;
use crate::types::*;

pub type R<T> = Result<T, ()>;

pub struct FnItem {
    pub def: Rc<FnDef>,
    pub module: usize,
}

pub struct FnCtx {
    pub locals: Vec<LocalInfo>,
    pub scopes: Vec<HashMap<String, LocalId>>,
    pub ret: Option<Type>,
    pub ret_declared: bool,
    pub loop_depth: usize,
    pub is_main: bool,
    pub module: usize,
    pub inst: FuncId,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ctx {
    /// The value is consumed (assigned, returned, passed to an `own` parameter, stored).
    Value,
    /// The value is only read for the duration of the operation.
    Borrow,
}

pub struct Checker<'a> {
    pub mods: &'a [LoadedModule],
    pub diags: &'a mut Diagnostics,
    pub structs: Vec<StructInfo>,
    pub struct_ids: HashMap<String, StructId>,
    pub fns: Vec<FnItem>,
    pub fn_index: HashMap<(usize, String), usize>,
    pub instances: HashMap<(usize, Vec<Type>), FuncId>,
    pub funcs: Vec<Option<HFunc>>,
    pub ret_of: Vec<Option<Type>>,
    pub stack: Vec<FnCtx>,
    pub plugins: &'a [PluginBinding],
}

pub fn check_program(mods: &[LoadedModule], plugins: &[PluginBinding], diags: &mut Diagnostics) -> Option<Program> {
    let mut c = Checker {
        mods,
        diags,
        structs: vec![],
        struct_ids: HashMap::new(),
        fns: vec![],
        fn_index: HashMap::new(),
        instances: HashMap::new(),
        funcs: vec![],
        ret_of: vec![],
        stack: vec![],
        plugins,
    };
    c.run()
}

pub fn he(kind: HKind, ty: Type, span: Span) -> HExpr {
    HExpr { kind, ty, span }
}

fn is_int_lit(e: &Expr) -> bool {
    matches!(e.kind, ExprKind::Int(_))
}

impl<'a> Checker<'a> {
    pub fn cx(&mut self) -> &mut FnCtx {
        self.stack.last_mut().unwrap()
    }

    pub fn tn(&self, t: &Type) -> String {
        display(t, &self.structs)
    }

    pub fn error(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    pub fn fail<T>(&mut self, d: Diagnostic) -> R<T> {
        self.diags.push(d);
        Err(())
    }

    fn run(&mut self) -> Option<Program> {
        // Structs: register names first (they may refer to each other), then resolve fields.
        for m in self.mods.iter() {
            for it in &m.ast.items {
                if let Item::Struct(s) = it {
                    if !self.struct_ids.contains_key(&s.name) {
                        self.struct_ids.insert(s.name.clone(), self.structs.len());
                        self.structs.push(StructInfo {
                            name: s.name.clone(),
                            fields: vec![],
                            span: s.name_span,
                            offsets: vec![],
                            size: 8,
                        });
                    }
                }
            }
        }
        for m in self.mods.iter() {
            for it in &m.ast.items {
                if let Item::Struct(s) = it {
                    let id = self.struct_ids[&s.name];
                    let mut fields = vec![];
                    for f in &s.fields {
                        if let Ok(t) = self.resolve_type(&f.ty) {
                            fields.push((f.name.clone(), t));
                        }
                    }
                    self.structs[id].fields = fields;
                }
            }
        }
        for i in 0..self.structs.len() {
            self.structs[i].compute_layout();
        }
        for (mi, m) in self.mods.iter().enumerate() {
            for it in &m.ast.items {
                if let Item::Fn(f) = it {
                    self.fn_index.insert((mi, f.name.clone()), self.fns.len());
                    self.fns.push(FnItem { def: Rc::new(f.clone()), module: mi });
                }
            }
        }

        // Main script.
        let main_id = self.new_func_slot();
        self.stack.push(FnCtx {
            locals: vec![],
            scopes: vec![HashMap::new()],
            ret: Some(Type::None),
            ret_declared: true,
            loop_depth: 0,
            is_main: true,
            module: 0,
            inst: main_id,
        });
        let mut body = vec![];
        let root = &self.mods[0].ast;
        let mut script: Vec<Stmt> = vec![];
        for it in &root.items {
            if let Item::Stmt(s) = it {
                script.push(s.clone());
            }
        }
        self.stmts_into(&script, &mut body);
        let cx = self.stack.pop().unwrap();
        let span = script.first().map(|s| s.span).unwrap_or_default();
        self.funcs[main_id] = Some(HFunc {
            name: "main".into(),
            params: vec![],
            locals: cx.locals,
            ret: Type::None,
            body,
            span,
            is_main: true,
        });

        // Check every function that needs no inference even when it is never called.
        for fidx in 0..self.fns.len() {
            let def = self.fns[fidx].def.clone();
            if def.params.iter().all(|p| p.ty.is_some()) {
                let mut tys = vec![];
                let mut ok = true;
                for p in &def.params {
                    match self.resolve_type(p.ty.as_ref().unwrap()) {
                        Ok(t) => tys.push(t),
                        Err(_) => ok = false,
                    }
                }
                if ok {
                    let _ = self.instantiate(fidx, tys, def.span);
                }
            }
        }

        if self.diags.has_errors() {
            return None;
        }
        let funcs: Vec<HFunc> = std::mem::take(&mut self.funcs).into_iter().map(|f| f.expect("function was checked")).collect();
        Some(Program { funcs, structs: std::mem::take(&mut self.structs), main: main_id, plugins: self.plugins.to_vec() })
    }

    fn new_func_slot(&mut self) -> FuncId {
        self.funcs.push(None);
        self.ret_of.push(None);
        self.funcs.len() - 1
    }

    // ---------------------------------------------------------------- types
    pub fn resolve_type(&mut self, t: &TypeExpr) -> R<Type> {
        if let Some(i) = IntTy::from_name(&t.name) {
            return self.no_args(t, Type::Int(i));
        }
        match t.name.as_str() {
            "Float" | "f64" => self.no_args(t, Type::Float),
            "Bool" => self.no_args(t, Type::Bool),
            "String" => self.no_args(t, Type::Str),
            "None" => self.no_args(t, Type::None),
            "List" | "Result" => {
                if t.args.len() != 1 {
                    return self.fail(
                        Diagnostic::error(t.span, format!("{} needs exactly one type argument.", t.name))
                            .help(format!("For example: {}[Int]", t.name)),
                    );
                }
                let inner = self.resolve_type(&t.args[0])?;
                Ok(if t.name == "List" { Type::List(Box::new(inner)) } else { Type::Result(Box::new(inner)) })
            }
            n => {
                if let Some(id) = self.struct_ids.get(n) {
                    return self.no_args(t, Type::Struct(*id));
                }
                self.fail(Diagnostic::error(t.span, format!("I don't know a type called '{}'.", n)))
            }
        }
    }

    fn no_args(&mut self, t: &TypeExpr, ty: Type) -> R<Type> {
        if !t.args.is_empty() {
            return self.fail(Diagnostic::error(t.span, format!("The type {} does not take type arguments.", t.name)));
        }
        Ok(ty)
    }

    // ----------------------------------------------------------- functions
    pub fn instantiate(&mut self, fidx: usize, arg_tys: Vec<Type>, call_span: Span) -> R<FuncId> {
        let key = (fidx, arg_tys.clone());
        if let Some(id) = self.instances.get(&key) {
            return Ok(*id);
        }
        let def = self.fns[fidx].def.clone();
        let module = self.fns[fidx].module;
        let id = self.new_func_slot();
        self.instances.insert(key, id);

        let declared_ret = match &def.ret {
            Some(t) => Some(self.resolve_type(t)?),
            None => None,
        };
        self.ret_of[id] = declared_ret.clone();
        let nparams_before = self.diags.error_count();
        self.stack.push(FnCtx {
            locals: vec![],
            scopes: vec![HashMap::new()],
            ret: declared_ret.clone(),
            ret_declared: declared_ret.is_some(),
            loop_depth: 0,
            is_main: false,
            module,
            inst: id,
        });
        let mut params = vec![];
        for (p, ty) in def.params.iter().zip(arg_tys.iter()) {
            let kind = match p.mode {
                ParamMode::Own => LocalKind::ParamOwn,
                ParamMode::Mut => {
                    if ty.is_copy() {
                        self.error(
                            Diagnostic::error(p.span, format!("A `mut` parameter must be a String, List or struct, but '{}' is {}.", p.name, self.tn(ty)))
                                .help("Numbers and booleans are copied into functions. Return the new value instead."),
                        );
                    }
                    LocalKind::ParamMut
                }
                ParamMode::Borrow => {
                    if ty.is_copy() {
                        LocalKind::Var
                    } else {
                        LocalKind::ParamBorrow
                    }
                }
            };
            let lid = self.add_local(&p.name, ty.clone(), kind, p.span);
            params.push(lid);
        }
        let mut body = vec![];
        self.stmts_into(&def.body, &mut body);
        let cx = self.stack.pop().unwrap();
        let ret = cx.ret.unwrap_or(Type::None);
        self.ret_of[id] = Some(ret.clone());
        if ret != Type::None && ret != Type::Error && !always_returns(&body) && self.diags.error_count() == nparams_before {
            self.error(
                Diagnostic::error(def.name_span, format!("The function '{}' can reach its end without returning a value.", def.name))
                    .help(format!("Every path through the function must `return` a {} (add a `return` at the end or in the `else` branch).", self.tn(&ret))),
            );
        }
        let _ = call_span;
        self.funcs[id] = Some(HFunc {
            name: def.name.clone(),
            params,
            locals: cx.locals,
            ret,
            body,
            span: def.span,
            is_main: false,
        });
        Ok(id)
    }

    pub fn add_local(&mut self, name: &str, ty: Type, kind: LocalKind, span: Span) -> LocalId {
        let cx = self.cx();
        cx.locals.push(LocalInfo { name: name.to_string(), ty, kind, span });
        let id = cx.locals.len() - 1;
        cx.scopes.last_mut().unwrap().insert(name.to_string(), id);
        id
    }

    pub fn lookup(&mut self, name: &str) -> Option<LocalId> {
        self.cx().scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    // ------------------------------------------------------------ statements
    pub fn stmts_into(&mut self, stmts: &[Stmt], out: &mut Vec<HStmt>) {
        for s in stmts {
            let _ = self.stmt(s, out);
        }
    }

    fn block(&mut self, stmts: &[Stmt]) -> Vec<HStmt> {
        self.cx().scopes.push(HashMap::new());
        let mut out = vec![];
        self.stmts_into(stmts, &mut out);
        self.cx().scopes.pop();
        out
    }

    fn cond(&mut self, e: &Expr, what: &str) -> R<HExpr> {
        let c = self.expr(e, Some(&Type::Bool), Ctx::Borrow)?;
        if c.ty != Type::Bool && c.ty != Type::Error {
            let tn = self.tn(&c.ty);
            let mut d = Diagnostic::error(e.span, format!("The condition of {} must be a Bool, but this is {}.", what, tn))
                .label("expected True or False");
            if c.ty.is_numeric() {
                d = d.help("Compare explicitly, for example `x != 0`.");
            }
            return self.fail(d);
        }
        Ok(c)
    }

    fn stmt(&mut self, s: &Stmt, out: &mut Vec<HStmt>) -> R<()> {
        let span = s.span;
        match &s.kind {
            StmtKind::Pass => {}
            StmtKind::Expr(e) => {
                let h = self.expr(e, None, Ctx::Value)?;
                out.push(HStmt { kind: HStmtKind::Expr(h), span });
            }
            StmtKind::Assign { target, ty, value } => self.assign(target, ty.as_ref(), value, span, out)?,
            StmtKind::AugAssign { target, op, value } => {
                let bin = Expr {
                    kind: ExprKind::Binary(*op, Box::new(target.clone()), Box::new(value.clone())),
                    span: target.span.to(value.span),
                };
                self.assign(target, None, &bin, span, out)?;
            }
            StmtKind::If { branches, else_body } => {
                // Build nested ifs from the back.
                let mut hb: Vec<(HExpr, Vec<HStmt>)> = vec![];
                let mut failed = false;
                for (c, b) in branches {
                    let hc = self.cond(c, "an `if`/`elif`");
                    let hbody = self.block(b);
                    match hc {
                        Ok(hc) => hb.push((hc, hbody)),
                        Err(_) => failed = true,
                    }
                }
                let mut tail = match else_body {
                    Some(b) => self.block(b),
                    None => vec![],
                };
                if failed {
                    return Err(());
                }
                while let Some((c, b)) = hb.pop() {
                    tail = vec![HStmt { kind: HStmtKind::If(c, b, tail), span }];
                }
                out.extend(tail);
            }
            StmtKind::While { cond, body } => {
                let c = self.cond(cond, "a `while` loop");
                self.cx().loop_depth += 1;
                let b = self.block(body);
                self.cx().loop_depth -= 1;
                out.push(HStmt { kind: HStmtKind::While(c?, b), span });
            }
            StmtKind::For { var, var_span, iter, body } => self.for_stmt(var, *var_span, iter, body, span, out)?,
            StmtKind::Break => {
                if self.cx().loop_depth == 0 {
                    return self.fail(Diagnostic::error(span, "`break` can only be used inside a loop."));
                }
                out.push(HStmt { kind: HStmtKind::Break, span });
            }
            StmtKind::Continue => {
                if self.cx().loop_depth == 0 {
                    return self.fail(Diagnostic::error(span, "`continue` can only be used inside a loop."));
                }
                out.push(HStmt { kind: HStmtKind::Continue, span });
            }
            StmtKind::Return(e) => self.return_stmt(e.as_ref(), span, out)?,
        }
        Ok(())
    }

    fn return_stmt(&mut self, e: Option<&Expr>, span: Span, out: &mut Vec<HStmt>) -> R<()> {
        if self.cx().is_main {
            return self.fail(
                Diagnostic::error(span, "`return` can only be used inside a function.")
                    .help("To stop the program early, call exit(0)."),
            );
        }
        let expected = self.cx().ret.clone();
        let val = match e {
            Some(e) => Some(self.expr(e, expected.as_ref(), Ctx::Value)?),
            None => None,
        };
        let vty = val.as_ref().map(|v| v.ty.clone()).unwrap_or(Type::None);
        let ret_span = val.as_ref().map(|v| v.span).unwrap_or(span);
        match expected {
            Some(r) => {
                if r != vty && vty != Type::Error && r != Type::Error {
                    let declared = self.cx().ret_declared;
                    let (rn, vn) = (self.tn(&r), self.tn(&vty));
                    let mut d = Diagnostic::error(
                        ret_span,
                        if declared {
                            format!("This function is declared to return {}, but this returns {}.", rn, vn)
                        } else {
                            format!("Inconsistent return types: an earlier `return` gives {}, but this one gives {}.", rn, vn)
                        },
                    );
                    if vty == Type::None && e.is_none() {
                        d = d.help(format!("Return a {} value here.", rn));
                    }
                    return self.fail(d);
                }
            }
            None => {
                self.cx().ret = Some(vty.clone());
                let id = self.cx().inst;
                self.ret_of[id] = Some(vty.clone());
            }
        }
        out.push(HStmt { kind: HStmtKind::Return(val), span });
        Ok(())
    }

    fn for_stmt(&mut self, var: &str, var_span: Span, iter: &ForIter, body: &[Stmt], span: Span, out: &mut Vec<HStmt>) -> R<()> {
        match iter {
            ForIter::Range { lo, hi, inclusive } => {
                let (hlo, hhi) = if is_int_lit(lo) && !is_int_lit(hi) {
                    let h = self.expr(hi, None, Ctx::Borrow)?;
                    let l = self.expr(lo, Some(&h.ty), Ctx::Borrow)?;
                    (l, h)
                } else {
                    let l = self.expr(lo, None, Ctx::Borrow)?;
                    let h = self.expr(hi, Some(&l.ty), Ctx::Borrow)?;
                    (l, h)
                };
                if !hlo.ty.is_int() || hlo.ty != hhi.ty {
                    let (a, b) = (self.tn(&hlo.ty), self.tn(&hhi.ty));
                    let msg = if hlo.ty.is_int() && hhi.ty.is_int() {
                        format!("Both ends of a range must have the same integer type, but they are {} and {}.", a, b)
                    } else {
                        format!("A range needs whole numbers at both ends, but this is {} .. {}.", a, b)
                    };
                    return self.fail(Diagnostic::error(lo.span.to(hi.span), msg).help("For Float values use a `while` loop."));
                }
                self.cx().scopes.push(HashMap::new());
                let v = self.add_local(var, hlo.ty.clone(), LocalKind::Var, var_span);
                self.cx().loop_depth += 1;
                let b = self.block(body);
                self.cx().loop_depth -= 1;
                self.cx().scopes.pop();
                out.push(HStmt { kind: HStmtKind::ForRange { var: v, lo: hlo, hi: hhi, inclusive: *inclusive, body: b }, span });
            }
            ForIter::Expr(e) => {
                let mut list = self.expr(e, None, Ctx::Borrow)?;
                let elem = match &list.ty {
                    Type::List(t) => (**t).clone(),
                    Type::Error => return Err(()),
                    other => {
                        let tn = self.tn(other);
                        return self.fail(
                            Diagnostic::error(e.span, format!("You can only loop over a range or a List, but this is {}.", tn))
                                .help("Use a range like `for i in 0..10:` or a list like `for x in numbers:`."),
                        );
                    }
                };
                // Keep temporaries alive for the whole loop by naming them.
                if !is_place(&list) {
                    let tmp = self.add_local("<iter>", list.ty.clone(), LocalKind::Var, e.span);
                    let lty = list.ty.clone();
                    out.push(HStmt { kind: HStmtKind::Let(tmp, list), span });
                    list = he(HKind::Local(tmp, UseMode::Borrow), lty, e.span);
                }
                self.cx().scopes.push(HashMap::new());
                let kind = if elem.is_copy() { LocalKind::Var } else { LocalKind::LoopRef };
                let v = self.add_local(var, elem, kind, var_span);
                self.cx().loop_depth += 1;
                let b = self.block(body);
                self.cx().loop_depth -= 1;
                self.cx().scopes.pop();
                out.push(HStmt { kind: HStmtKind::ForList { var: v, list, body: b }, span });
            }
        }
        Ok(())
    }

    fn assign(&mut self, target: &Expr, ann: Option<&TypeExpr>, value: &Expr, span: Span, out: &mut Vec<HStmt>) -> R<()> {
        let ann_ty = match ann {
            Some(t) => Some(self.resolve_type(t)?),
            None => None,
        };
        match &target.kind {
            ExprKind::Name(name) => {
                if let Some(id) = self.lookup(name) {
                    let (lty, lkind) = {
                        let l = &self.cx().locals[id];
                        (l.ty.clone(), l.kind)
                    };
                    if let Some(a) = &ann_ty {
                        if *a != lty {
                            let (an, ln) = (self.tn(a), self.tn(&lty));
                            return self.fail(Diagnostic::error(
                                target.span,
                                format!("'{}' already exists with type {}, so it cannot be declared as {}.", name, ln, an),
                            ));
                        }
                    }
                    let v = self.expr(value, Some(&lty), Ctx::Value);
                    let v = v?;
                    if lkind == LocalKind::ParamBorrow || lkind == LocalKind::LoopRef {
                        let what = if lkind == LocalKind::LoopRef { "a loop variable that borrows a list element" } else { "a read-only parameter" };
                        return self.fail(
                            Diagnostic::error(target.span, format!("Cannot assign to '{}' because it is {}.", name, what))
                                .help("Make a new variable with `copy(...)`, or declare the parameter `mut` / `own`."),
                        );
                    }
                    if lkind == LocalKind::ParamMut {
                        return self.fail(
                            Diagnostic::error(target.span, format!("Cannot replace the whole `mut` parameter '{}'.", name))
                                .help("Modify its contents instead (for example `append(items, x)` or `item.field = x`)."),
                        );
                    }
                    if v.ty != lty && v.ty != Type::Error {
                        return self.type_mismatch(value.span, &lty, &v, &format!("'{}'", name));
                    }
                    let place = he(HKind::Local(id, UseMode::Borrow), lty, target.span);
                    out.push(HStmt { kind: HStmtKind::Assign(place, v), span });
                } else {
                    let v = self.expr(value, ann_ty.as_ref(), Ctx::Value);
                    let v = match v {
                        Ok(v) => v,
                        Err(_) => {
                            // Declare the name anyway so later uses don't cascade into more errors.
                            let t = ann_ty.clone().unwrap_or(Type::Error);
                            self.add_local(name, t, LocalKind::Var, target.span);
                            return Err(());
                        }
                    };
                    if let Some(a) = &ann_ty {
                        if *a != v.ty && v.ty != Type::Error {
                            self.add_local(name, a.clone(), LocalKind::Var, target.span);
                            return self.type_mismatch(value.span, a, &v, &format!("'{}'", name));
                        }
                    }
                    let ty = ann_ty.unwrap_or_else(|| v.ty.clone());
                    let id = self.add_local(name, ty, LocalKind::Var, target.span);
                    out.push(HStmt { kind: HStmtKind::Let(id, v), span });
                }
            }
            ExprKind::Field { .. } | ExprKind::Index { .. } => {
                if ann.is_some() {
                    return self.fail(Diagnostic::error(target.span, "A type annotation can only be given when creating a variable."));
                }
                let place = self.expr(target, None, Ctx::Borrow)?;
                self.check_mut_place(&place, target.span, "assign to")?;
                let v = self.expr(value, Some(&place.ty), Ctx::Value)?;
                if v.ty != place.ty && v.ty != Type::Error && place.ty != Type::Error {
                    return self.type_mismatch(value.span, &place.ty.clone(), &v, "this place");
                }
                out.push(HStmt { kind: HStmtKind::Assign(place, v), span });
            }
            _ => {
                return self.fail(
                    Diagnostic::error(target.span, "You can only assign to a variable, a field (`a.b`) or a list element (`a[i]`).")
                );
            }
        }
        Ok(())
    }

    pub fn type_mismatch<T>(&mut self, span: Span, expected: &Type, found: &HExpr, what: &str) -> R<T> {
        let (en, fnn) = (self.tn(expected), self.tn(&found.ty));
        let mut d = Diagnostic::error(span, format!("{} should be {}, but this is {}.", capitalize(what), en, fnn))
            .label(format!("expected {}", en));
        if *expected == Type::Float && found.ty.is_int() {
            d = d.help("Flash never converts numbers silently. Write a Float literal like `5.0`, or convert with `float(x)`.");
        } else if expected.is_int() && found.ty == Type::Float {
            d = d.help("Convert explicitly with `int(x)` (this truncates toward zero).");
        } else if expected.is_int() && found.ty.is_int() {
            d = d.help(format!("Convert explicitly, for example `{}(x)`.", self.tn(expected).to_lowercase()));
        } else if *expected == Type::Str {
            d = d.help("Convert a value to text with `str(x)`.");
        }
        self.fail(d)
    }

    /// The root variable of a place expression (`a`, `a.b`, `a[i].c`).
    pub fn place_root(e: &HExpr) -> Option<LocalId> {
        match &e.kind {
            HKind::Local(id, _) => Some(*id),
            HKind::Field(b, _) | HKind::Index(b, _) => Self::place_root(b),
            _ => None,
        }
    }

    /// Verify that a place may be modified in place.
    pub fn check_mut_place(&mut self, place: &HExpr, span: Span, verb: &str) -> R<()> {
        let Some(root) = Self::place_root(place) else {
            return self.fail(Diagnostic::error(span, format!("Cannot {} a temporary value.", verb)).help("Store it in a variable first."));
        };
        let (name, kind) = {
            let l = &self.cx().locals[root];
            (l.name.clone(), l.kind)
        };
        match kind {
            LocalKind::ParamBorrow => self.fail(
                Diagnostic::error(span, format!("Cannot modify '{}' because it is a read-only parameter.", name))
                    .help(format!("Declare the parameter as `mut {}` to allow changes.", name)),
            ),
            LocalKind::LoopRef => self.fail(Diagnostic::error(
                span,
                format!("Cannot modify '{}' because it only borrows an element of the list being looped over.", name),
            )),
            _ => Ok(()),
        }
    }

    // ----------------------------------------------------------- expressions
    pub fn expr(&mut self, e: &Expr, expected: Option<&Type>, ctx: Ctx) -> R<HExpr> {
        let span = e.span;
        match &e.kind {
            ExprKind::Int(v) => {
                let ty = match expected {
                    Some(Type::Int(t)) => *t,
                    _ => INT,
                };
                if *v < ty.min() || *v > ty.max() {
                    return self.fail(
                        Diagnostic::error(span, format!("The number {} does not fit in {}.", v, ty.name()))
                            .help(format!("{} can hold {} to {}.", ty.name(), ty.min(), ty.max())),
                    );
                }
                Ok(he(HKind::Int(*v as i64), Type::Int(ty), span))
            }
            ExprKind::Float(v) => Ok(he(HKind::Float(*v), Type::Float, span)),
            ExprKind::Str(s) => Ok(he(HKind::Str(s.clone()), Type::Str, span)),
            ExprKind::Bool(b) => Ok(he(HKind::Bool(*b), Type::Bool, span)),
            ExprKind::None => Ok(he(HKind::None, Type::None, span)),
            ExprKind::Name(n) => {
                let Some(id) = self.lookup(n) else {
                    // Reported by the resolver; stay quiet here.
                    return Err(());
                };
                let (ty, kind) = {
                    let l = &self.cx().locals[id];
                    (l.ty.clone(), l.kind)
                };
                if ty == Type::Error {
                    return Err(());
                }
                if ty.is_copy() {
                    return Ok(he(HKind::Local(id, UseMode::Copy), ty, span));
                }
                if ctx == Ctx::Value {
                    if !kind.owns() {
                        return self.fail(
                            Diagnostic::error(span, format!("Cannot move '{}' out because it is only borrowed here.", n))
                                .help(format!("Use `copy({})` to make an independent copy, or declare the parameter `own {}`.", n, n)),
                        );
                    }
                    Ok(he(HKind::Local(id, UseMode::Move), ty, span))
                } else {
                    Ok(he(HKind::Local(id, UseMode::Borrow), ty, span))
                }
            }
            ExprKind::Unary(op, a) => self.unary(*op, a, span, expected),
            ExprKind::Binary(op, a, b) => self.binary(*op, a, b, span, expected),
            ExprKind::List(elems) => self.list_lit(elems, expected, span),
            ExprKind::Field { base, name, name_span } => {
                let hb = self.expr(base, None, Ctx::Borrow)?;
                let Type::Struct(sid) = hb.ty else {
                    if hb.ty == Type::Error {
                        return Err(());
                    }
                    let tn = self.tn(&hb.ty);
                    return self.fail(Diagnostic::error(*name_span, format!("{} has no field '{}'; only structs have fields.", tn, name)));
                };
                let Some(idx) = self.structs[sid].fields.iter().position(|(n, _)| n == name) else {
                    let cands: Vec<&str> = self.structs[sid].fields.iter().map(|(n, _)| n.as_str()).collect();
                    let mut d = Diagnostic::error(*name_span, format!("The struct {} has no field '{}'.", self.structs[sid].name, name));
                    if let Some(s) = suggest(name, cands.iter().copied()) {
                        d = d.help(format!("Did you mean '{}'?", s));
                    } else {
                        d = d.help(format!("Its fields are: {}.", cands.join(", ")));
                    }
                    return self.fail(d);
                };
                let fty = self.structs[sid].fields[idx].1.clone();
                self.check_value_use(&fty, ctx, span, &format!("the field '{}'", name))?;
                Ok(he(HKind::Field(Box::new(hb), idx), fty, span))
            }
            ExprKind::Index { base, index } => {
                let hb = self.expr(base, None, Ctx::Borrow)?;
                let elem = match &hb.ty {
                    Type::List(t) => (**t).clone(),
                    Type::Error => return Err(()),
                    Type::Str => {
                        return self.fail(
                            Diagnostic::error(span, "Strings cannot be indexed directly.")
                                .help("Use `byte_at(s, i)` for a byte value or `substr(s, start, length)` for a piece of text."),
                        )
                    }
                    other => {
                        let tn = self.tn(other);
                        return self.fail(Diagnostic::error(span, format!("Cannot index into {}; only lists support `[...]`.", tn)));
                    }
                };
                let hi = self.expr(index, None, Ctx::Borrow)?;
                if !hi.ty.is_int() {
                    let tn = self.tn(&hi.ty);
                    return self.fail(Diagnostic::error(index.span, format!("A list index must be an integer, but this is {}.", tn)));
                }
                self.check_value_use(&elem, ctx, span, "this list element")?;
                Ok(he(HKind::Index(Box::new(hb), Box::new(hi)), elem, span))
            }
            ExprKind::Try(inner) => self.try_expr(inner, span),
            ExprKind::Call { callee, args } => self.call(callee, args, span, expected, ctx),
        }
    }

    /// Moving an owned value out of a field / list slot would leave a hole behind.
    fn check_value_use(&mut self, ty: &Type, ctx: Ctx, span: Span, what: &str) -> R<()> {
        if ctx == Ctx::Value && ty.is_owned() {
            return self.fail(
                Diagnostic::error(span, format!("Cannot move {} out of its container.", what))
                    .help("Use `copy(...)` to make an independent copy, or read it where a borrow is enough (for example inside `say(...)`)."),
            );
        }
        Ok(())
    }

    fn try_expr(&mut self, inner: &Expr, span: Span) -> R<HExpr> {
        let h = self.expr(inner, None, Ctx::Value)?;
        let Type::Result(t) = &h.ty else {
            if h.ty == Type::Error {
                return Err(());
            }
            let tn = self.tn(&h.ty);
            return self.fail(
                Diagnostic::error(span, format!("The `?` operator needs a Result, but this is {}.", tn))
                    .help("`?` unwraps the value of a fallible call such as read_file(path), or returns its error from the current function."),
            );
        };
        let t = (**t).clone();
        let (is_main, ret, declared) = {
            let cx = self.cx();
            (cx.is_main, cx.ret.clone(), cx.ret_declared)
        };
        if !is_main {
            match ret {
                Some(Type::Result(_)) if declared => {}
                _ => {
                    return self.fail(
                        Diagnostic::error(span, "`?` can only be used in a function that is declared to return a Result.")
                            .help("Add a return type, for example `fn load(path) -> Result[String]:`. A top-level script may use `?` freely; an error ends the program with a message."),
                    )
                }
            }
        }
        Ok(he(HKind::Try(Box::new(h)), t, span))
    }

    fn list_lit(&mut self, elems: &[Expr], expected: Option<&Type>, span: Span) -> R<HExpr> {
        let exp_elem = match expected {
            Some(Type::List(t)) => Some((**t).clone()),
            _ => None,
        };
        if elems.is_empty() {
            return match exp_elem {
                Some(t) => Ok(he(HKind::List(vec![]), Type::List(Box::new(t)), span)),
                None => self.fail(
                    Diagnostic::error(span, "I can't tell what kind of list this empty list is.")
                        .help("Add a type annotation, for example `items: List[Int] = []`."),
                ),
            };
        }
        let mut hs: Vec<HExpr> = vec![];
        let mut elem_ty = exp_elem;
        for el in elems {
            let h = self.expr(el, elem_ty.as_ref(), Ctx::Value)?;
            match &elem_ty {
                None => elem_ty = Some(h.ty.clone()),
                Some(t) => {
                    if *t != h.ty && h.ty != Type::Error {
                        let t = t.clone();
                        return self.type_mismatch(el.span, &t, &h, "Every list element");
                    }
                }
            }
            hs.push(h);
        }
        Ok(he(HKind::List(hs), Type::List(Box::new(elem_ty.unwrap())), span))
    }

    fn unary(&mut self, op: UnOp, a: &Expr, span: Span, expected: Option<&Type>) -> R<HExpr> {
        let h = self.expr(a, if op == UnOp::Not { Some(&Type::Bool) } else { expected }, Ctx::Borrow)?;
        let ok = match op {
            UnOp::Neg => matches!(h.ty, Type::Float) || matches!(h.ty, Type::Int(i) if i.signed),
            UnOp::Not => h.ty == Type::Bool,
            UnOp::BitNot => h.ty.is_int(),
        };
        if !ok {
            let tn = self.tn(&h.ty);
            let (what, help) = match op {
                UnOp::Neg => ("negate", "Only signed integers and Floats can be negated."),
                UnOp::Not => ("apply `not` to", "`not` needs a Bool."),
                UnOp::BitNot => ("apply `~` to", "`~` needs an integer."),
            };
            return self.fail(Diagnostic::error(span, format!("Cannot {} {}.", what, tn)).help(help));
        }
        let ty = h.ty.clone();
        Ok(he(HKind::Unary(op, Box::new(h)), ty, span))
    }

    fn binary(&mut self, op: BinOp, a: &Expr, b: &Expr, span: Span, expected: Option<&Type>) -> R<HExpr> {
        if matches!(op, BinOp::And | BinOp::Or) {
            let ha = self.expr(a, Some(&Type::Bool), Ctx::Borrow)?;
            let hb = self.expr(b, Some(&Type::Bool), Ctx::Borrow)?;
            for (h, ex) in [(&ha, a), (&hb, b)] {
                if h.ty != Type::Bool {
                    let tn = self.tn(&h.ty);
                    return self.fail(
                        Diagnostic::error(ex.span, format!("`{}` needs Bool on both sides, but this is {}.", op.symbol(), tn))
                            .help("Compare first, for example `x != 0 and y > 1`."),
                    );
                }
            }
            return Ok(he(HKind::Logic(op == BinOp::And, Box::new(ha), Box::new(hb)), Type::Bool, span));
        }
        // Integer literals adapt to the type of the other operand.
        let arith_expected = if op.is_comparison() { None } else { expected };
        let (ha, hb) = if is_int_lit(a) && !is_int_lit(b) {
            let hb = self.expr(b, arith_expected, Ctx::Borrow)?;
            let ha = self.expr(a, Some(&hb.ty), Ctx::Borrow)?;
            (ha, hb)
        } else {
            let ha = self.expr(a, arith_expected, Ctx::Borrow)?;
            let hb = self.expr(b, Some(&ha.ty), Ctx::Borrow)?;
            (ha, hb)
        };
        if ha.ty == Type::Error || hb.ty == Type::Error {
            return Err(());
        }
        let (lt, rt) = (ha.ty.clone(), hb.ty.clone());
        let valid = if lt != rt {
            false
        } else {
            match op {
                BinOp::Add => lt.is_numeric() || lt == Type::Str,
                BinOp::Sub | BinOp::Mul | BinOp::Div => lt.is_numeric(),
                BinOp::Rem | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => lt.is_int(),
                BinOp::Eq | BinOp::Ne => lt.is_numeric() || lt == Type::Bool || lt == Type::Str,
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => lt.is_numeric() || lt == Type::Str,
                BinOp::And | BinOp::Or => unreachable!(),
            }
        };
        if !valid {
            let verb = match op {
                BinOp::Add => "add",
                BinOp::Sub => "subtract",
                BinOp::Mul => "multiply",
                BinOp::Div => "divide",
                BinOp::Rem => "take the remainder of",
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => "compare",
                _ => "combine",
            };
            let (ln, rn) = (self.tn(&lt), self.tn(&rt));
            let msg = if matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr) {
                format!("Cannot apply '{}' to {} and {}.", op.symbol(), ln, rn)
            } else {
                format!("Cannot {} {} and {}.", verb, ln, rn)
            };
            let mut d = Diagnostic::error(b.span, msg).label(if lt.is_numeric() { "expected a compatible numeric value".to_string() } else { format!("expected {}", ln) });
            d = d.note(a.span, format!("this is {}", ln));
            if lt != rt {
                if (lt == Type::Float && rt.is_int()) || (lt.is_int() && rt == Type::Float) {
                    d = d.help("Flash never mixes Int and Float silently. Convert one side: `float(x)` or `int(x)`, or write the literal as `2.0`.");
                } else if lt == Type::Str || rt == Type::Str {
                    d = d.help("Convert the number to text with `str(x)`, or use an f-string like f\"value: {x}\".");
                } else if lt.is_int() && rt.is_int() {
                    d = d.help("The two integers have different sizes or signs. Convert one explicitly, for example `i64(x)`.");
                }
            } else if matches!(op, BinOp::Eq | BinOp::Ne) {
                d = d.help("Only numbers, Bools and Strings can be compared.");
            } else if lt == Type::Float && op == BinOp::Rem {
                d = d.help("`%` works on integers only.");
            }
            return self.fail(d);
        }
        let ty = if op.is_comparison() { Type::Bool } else { lt };
        Ok(he(HKind::Binary(op, Box::new(ha), Box::new(hb)), ty, span))
    }

    // ---------------------------------------------------------------- calls
    fn call(&mut self, callee: &Expr, args: &[Arg], span: Span, expected: Option<&Type>, ctx: Ctx) -> R<HExpr> {
        match &callee.kind {
            ExprKind::Name(n) => {
                let module = self.cx().module;
                if let Some(&fidx) = self.fn_index.get(&(module, n.clone())) {
                    let list: Vec<(Option<&str>, &Expr)> = args.iter().map(|a| (a.name.as_deref(), &a.value)).collect();
                    return self.call_user(fidx, &list, span);
                }
                if let Some(&sid) = self.struct_ids.get(n) {
                    return self.construct(sid, args, span);
                }
                if is_builtin(n) {
                    let list: Vec<&Expr> = args.iter().map(|a| &a.value).collect();
                    if let Some(a) = args.iter().find(|a| a.name.is_some()) {
                        return self.fail(Diagnostic::error(a.value.span, format!("The built-in function '{}' does not take named arguments.", n)));
                    }
                    return self.builtin(n, &list, span, expected, ctx);
                }
                Err(())
            }
            ExprKind::Field { base, name, .. } => {
                if let ExprKind::Name(m) = &base.kind {
                    if self.lookup(m).is_none() {
                        let module = self.cx().module;
                        if let Some((_, mi)) = self.mods[module].imports.iter().find(|(a, _)| a == m) {
                            let mi = *mi;
                            if let Some(&fidx) = self.fn_index.get(&(mi, name.clone())) {
                                let list: Vec<(Option<&str>, &Expr)> = args.iter().map(|a| (a.name.as_deref(), &a.value)).collect();
                                return self.call_user(fidx, &list, span);
                            }
                            return Err(());
                        }
                        if let Some((_, pi)) = self.mods[module].plugins.iter().find(|(a, _)| a == m) {
                            let pi = *pi;
                            return self.call_plugin(pi, name, args, span);
                        }
                    }
                }
                // Method-call syntax: receiver.f(args) == f(receiver, args)
                let module = self.cx().module;
                let mut list: Vec<(Option<&str>, &Expr)> = vec![(None, &**base)];
                list.extend(args.iter().map(|a| (a.name.as_deref(), &a.value)));
                if let Some(&fidx) = self.fn_index.get(&(module, name.clone())) {
                    return self.call_user(fidx, &list, span);
                }
                if is_builtin(name) {
                    let l2: Vec<&Expr> = list.iter().map(|(_, e)| *e).collect();
                    return self.builtin(name, &l2, span, expected, ctx);
                }
                Err(())
            }
            _ => Err(()),
        }
    }

    /// Match positional and keyword arguments to named parameters.
    pub fn bind_args<'e>(&mut self, what: &str, names: &[String], args: &[(Option<&str>, &'e Expr)], span: Span) -> R<Vec<&'e Expr>> {
        let mut slots: Vec<Option<&'e Expr>> = vec![None; names.len()];
        let mut pos = 0;
        for (name, e) in args {
            match name {
                None => {
                    if pos >= names.len() {
                        return self.fail(Diagnostic::error(
                            e.span,
                            format!("{} takes {} argument{}, but more were given.", what, names.len(), if names.len() == 1 { "" } else { "s" }),
                        ));
                    }
                    if slots[pos].is_some() {
                        return self.fail(Diagnostic::error(e.span, format!("The argument '{}' was given twice.", names[pos])));
                    }
                    slots[pos] = Some(e);
                    pos += 1;
                }
                Some(n) => {
                    let Some(i) = names.iter().position(|x| x == n) else {
                        let mut d = Diagnostic::error(e.span, format!("{} has no parameter named '{}'.", what, n));
                        if let Some(s) = suggest(n, names.iter().map(|s| s.as_str())) {
                            d = d.help(format!("Did you mean '{}'?", s));
                        }
                        return self.fail(d);
                    };
                    if slots[i].is_some() {
                        return self.fail(Diagnostic::error(e.span, format!("The argument '{}' was given twice.", n)));
                    }
                    slots[i] = Some(e);
                }
            }
        }
        let missing: Vec<&str> = names.iter().zip(slots.iter()).filter(|(_, s)| s.is_none()).map(|(n, _)| n.as_str()).collect();
        if !missing.is_empty() {
            return self.fail(Diagnostic::error(span, format!("{} is missing: {}.", what, missing.join(", "))));
        }
        Ok(slots.into_iter().map(|s| s.unwrap()).collect())
    }

    fn call_user(&mut self, fidx: usize, args: &[(Option<&str>, &Expr)], span: Span) -> R<HExpr> {
        let def = self.fns[fidx].def.clone();
        let names: Vec<String> = def.params.iter().map(|p| p.name.clone()).collect();
        let what = format!("The function '{}'", def.name);
        let bound = self.bind_args(&what, &names, args, span)?;
        let mut hargs = vec![];
        let mut tys = vec![];
        let mut failed = false;
        for (p, e) in def.params.iter().zip(bound.iter()) {
            let declared = match &p.ty {
                Some(t) => match self.resolve_type(t) {
                    Ok(t) => Some(t),
                    Err(_) => {
                        failed = true;
                        continue;
                    }
                },
                None => None,
            };
            let ctx = if p.mode == ParamMode::Own { Ctx::Value } else { Ctx::Borrow };
            let Ok(h) = self.expr(e, declared.as_ref(), ctx) else {
                failed = true;
                continue;
            };
            if let Some(d) = &declared {
                if *d != h.ty && h.ty != Type::Error {
                    let what = format!("The argument '{}' of '{}'", p.name, def.name);
                    let _ = self.type_mismatch::<()>(e.span, d, &h, &what);
                    failed = true;
                    continue;
                }
            }
            if p.mode == ParamMode::Mut {
                if self.check_mut_place(&h, e.span, "pass as `mut`").is_err() {
                    failed = true;
                    continue;
                }
                if h.ty.is_copy() {
                    let tn = self.tn(&h.ty);
                    self.error(Diagnostic::error(e.span, format!("A `mut` parameter must be a String, List or struct, but this is {}.", tn)));
                    failed = true;
                    continue;
                }
            }
            tys.push(h.ty.clone());
            hargs.push(h);
        }
        if failed {
            return Err(());
        }
        let nerr = self.diags.error_count();
        let id = self.instantiate(fidx, tys, span)?;
        if self.diags.error_count() > nerr {
            return Err(());
        }
        let Some(ret) = self.ret_of[id].clone() else {
            return self.fail(
                Diagnostic::error(span, format!("I can't work out what '{}' returns because it calls itself before any `return` that decides its type.", def.name))
                    .help(format!("Add a return type: `fn {}(...) -> Int:`.", def.name)),
            );
        };
        Ok(he(HKind::Call(id, hargs), ret, span))
    }

    fn construct(&mut self, sid: StructId, args: &[Arg], span: Span) -> R<HExpr> {
        let fields = self.structs[sid].fields.clone();
        let names: Vec<String> = fields.iter().map(|(n, _)| n.clone()).collect();
        let what = format!("The struct {}", self.structs[sid].name);
        let list: Vec<(Option<&str>, &Expr)> = args.iter().map(|a| (a.name.as_deref(), &a.value)).collect();
        let bound = self.bind_args(&what, &names, &list, span)?;
        let mut hs = vec![];
        let mut failed = false;
        for ((fname, fty), e) in fields.iter().zip(bound.iter()) {
            match self.expr(e, Some(fty), Ctx::Value) {
                Ok(h) => {
                    if h.ty != *fty && h.ty != Type::Error {
                        let what = format!("The field '{}'", fname);
                        let _ = self.type_mismatch::<()>(e.span, fty, &h, &what);
                        failed = true;
                    } else {
                        hs.push(h);
                    }
                }
                Err(_) => failed = true,
            }
        }
        if failed {
            return Err(());
        }
        Ok(he(HKind::Struct(hs), Type::Struct(sid), span))
    }

    fn call_plugin(&mut self, pi: usize, fname: &str, args: &[Arg], span: Span) -> R<HExpr> {
        let plugins = self.plugins;
        let p = &plugins[pi];
        let Some(fi) = p.funcs.iter().position(|f| f.name == fname) else {
            return Err(());
        };
        let f = &p.funcs[fi];
        let what = format!("The plugin function '{}.{}'", p.name, f.name);
        if let Some(a) = args.iter().find(|a| a.name.is_some()) {
            return self.fail(Diagnostic::error(a.value.span, format!("{} does not take named arguments.", what)));
        }
        if args.len() != f.params.len() {
            return self.fail(Diagnostic::error(
                span,
                format!("{} takes {} argument{}, but {} {} given.", what, f.params.len(), if f.params.len() == 1 { "" } else { "s" }, args.len(), if args.len() == 1 { "was" } else { "were" }),
            ));
        }
        let mut hs = vec![];
        let mut failed = false;
        for (a, pt) in args.iter().zip(f.params.iter()) {
            match self.expr(&a.value, Some(pt), Ctx::Borrow) {
                Ok(h) => {
                    if h.ty != *pt && h.ty != Type::Error {
                        let _ = self.type_mismatch::<()>(a.value.span, pt, &h, "A plugin argument");
                        failed = true;
                    } else {
                        hs.push(h)
                    }
                }
                Err(_) => failed = true,
            }
        }
        if failed {
            return Err(());
        }
        let rty = if f.fallible { Type::Result(Box::new(f.ret.clone())) } else { f.ret.clone() };
        Ok(he(HKind::PluginCall { plugin: pi, func: fi, args: hs }, rty, span))
    }
}

pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

pub fn is_place(e: &HExpr) -> bool {
    matches!(e.kind, HKind::Local(..) | HKind::Field(..) | HKind::Index(..))
}

fn contains_break(stmts: &[HStmt]) -> bool {
    stmts.iter().any(|s| match &s.kind {
        HStmtKind::Break => true,
        HStmtKind::If(_, a, b) => contains_break(a) || contains_break(b),
        // `break` inside a nested loop only leaves that loop.
        _ => false,
    })
}

pub fn always_returns(stmts: &[HStmt]) -> bool {
    match stmts.last() {
        None => false,
        Some(_) => stmts.iter().any(|s| match &s.kind {
            HStmtKind::Return(_) => true,
            HStmtKind::If(_, a, b) => always_returns(a) && always_returns(b),
            HStmtKind::While(c, body) => matches!(c.kind, HKind::Bool(true)) && !contains_break(body),
            _ => false,
        }),
    }
}
