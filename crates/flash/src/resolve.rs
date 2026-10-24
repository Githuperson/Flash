//! Name resolution: finds undefined variables/functions, duplicate definitions and
//! misplaced `break`/`continue` before type checking starts.
//!
//! Scoping rules (see LANGUAGE_SPEC.md): variables are declared by their first assignment and
//! live until the end of the enclosing indented block. Functions cannot see variables of the
//! top-level script.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::builtins::is_builtin;
use crate::diag::{suggest, Diagnostic, Diagnostics};
use crate::hir::PluginBinding;
use crate::module::LoadedModule;
use crate::source::Span;

struct Resolver<'a> {
    mods: &'a [LoadedModule],
    plugins: &'a [PluginBinding],
    diags: &'a mut Diagnostics,
    /// per module: function names
    fns: Vec<HashSet<String>>,
    structs: HashMap<String, Span>,
    cur: usize,
    scopes: Vec<HashSet<String>>,
    loop_depth: usize,
}

pub fn resolve(mods: &[LoadedModule], plugins: &[PluginBinding], diags: &mut Diagnostics) {
    let mut r = Resolver {
        mods,
        plugins,
        diags,
        fns: vec![],
        structs: HashMap::new(),
        cur: 0,
        scopes: vec![],
        loop_depth: 0,
    };
    r.run();
}

impl<'a> Resolver<'a> {
    fn err(&mut self, span: Span, msg: String) -> usize {
        self.diags.push(Diagnostic::error(span, msg));
        self.diags.items.len() - 1
    }

    fn run(&mut self) {
        // Pass 1: collect definitions.
        for (mi, m) in self.mods.iter().enumerate() {
            let mut names: HashMap<String, Span> = HashMap::new();
            for it in &m.ast.items {
                match it {
                    Item::Fn(f) => {
                        if let Some(prev) = names.get(&f.name) {
                            self.diags.push(
                                Diagnostic::error(f.name_span, format!("The function '{}' is defined twice.", f.name))
                                    .note(*prev, "first defined here")
                                    .help("Give one of them a different name."),
                            );
                        } else if is_builtin(&f.name) {
                            self.diags.push(
                                Diagnostic::error(
                                    f.name_span,
                                    format!("'{}' is a built-in function and cannot be redefined.", f.name),
                                )
                                .help("Choose a different name."),
                            );
                        } else {
                            names.insert(f.name.clone(), f.name_span);
                        }
                    }
                    Item::Struct(s) => {
                        if let Some(prev) = self.structs.get(&s.name) {
                            self.diags.push(
                                Diagnostic::error(s.name_span, format!("The struct '{}' is defined twice.", s.name))
                                    .note(*prev, "first defined here"),
                            );
                        } else {
                            self.structs.insert(s.name.clone(), s.name_span);
                        }
                        let mut seen = HashSet::new();
                        for f in &s.fields {
                            if !seen.insert(f.name.clone()) {
                                self.diags.push(Diagnostic::error(
                                    f.span,
                                    format!("The field '{}' appears twice in struct {}.", f.name, s.name),
                                ));
                            }
                        }
                    }
                    Item::Stmt(s) if mi != 0 => {
                        self.diags.push(
                            Diagnostic::error(s.span, "Imported modules can only contain `fn`, `struct` and `import`.")
                                .help("Put this statement inside a function."),
                        );
                    }
                    _ => {}
                }
            }
            self.fns.push(names.keys().cloned().collect());
        }
        // Pass 2: bodies.
        for mi in 0..self.mods.len() {
            self.cur = mi;
            let items = &self.mods[mi].ast.items;
            // top-level script scope
            self.scopes = vec![HashSet::new()];
            self.loop_depth = 0;
            for it in items {
                match it {
                    Item::Fn(f) => {
                        let saved = std::mem::take(&mut self.scopes);
                        let mut scope = HashSet::new();
                        let mut seen = HashSet::new();
                        for p in &f.params {
                            if !seen.insert(p.name.clone()) {
                                self.err(p.span, format!("The parameter '{}' appears twice.", p.name));
                            }
                            scope.insert(p.name.clone());
                            if let Some(t) = &p.ty {
                                self.type_expr(t);
                            }
                        }
                        if let Some(t) = &f.ret {
                            self.type_expr(t);
                        }
                        self.scopes = vec![scope];
                        self.block(&f.body);
                        self.scopes = saved;
                    }
                    Item::Struct(s) => {
                        for f in &s.fields {
                            self.type_expr(&f.ty);
                        }
                    }
                    Item::Stmt(s) => {
                        if mi == 0 {
                            self.stmt(s)
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn type_expr(&mut self, t: &TypeExpr) {
        use crate::types::IntTy;
        let known = IntTy::from_name(&t.name).is_some()
            || matches!(t.name.as_str(), "Float" | "f64" | "Bool" | "String" | "None" | "List" | "Result")
            || self.structs.contains_key(&t.name);
        if !known {
            let cands: Vec<&str> = ["Int", "Float", "Bool", "String", "None", "List", "Result", "i32", "u8"]
                .iter()
                .copied()
                .chain(self.structs.keys().map(|s| s.as_str()))
                .collect();
            let mut d = Diagnostic::error(t.span, format!("I don't know a type called '{}'.", t.name));
            if let Some(s) = suggest(&t.name, cands.into_iter()) {
                d = d.help(format!("Did you mean '{}'?", s));
            } else {
                d = d.help("Built-in types are Int, Float, Bool, String, None, List[T] and Result[T]; structs are also types.");
            }
            self.diags.push(d);
        }
        for a in &t.args {
            self.type_expr(a);
        }
    }

    fn declared(&self, n: &str) -> bool {
        self.scopes.iter().any(|s| s.contains(n))
    }

    fn block(&mut self, stmts: &[Stmt]) {
        self.scopes.push(HashSet::new());
        for s in stmts {
            self.stmt(s);
        }
        self.scopes.pop();
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Assign { target, ty, value } => {
                if let Some(t) = ty {
                    self.type_expr(t);
                }
                self.expr(value);
                match &target.kind {
                    ExprKind::Name(n) => {
                        if !self.declared(n) {
                            self.scopes.last_mut().unwrap().insert(n.clone());
                        }
                    }
                    _ => self.expr(target),
                }
            }
            StmtKind::AugAssign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            StmtKind::Expr(e) => self.expr(e),
            StmtKind::If { branches, else_body } => {
                for (c, b) in branches {
                    self.expr(c);
                    self.block(b);
                }
                if let Some(b) = else_body {
                    self.block(b);
                }
            }
            StmtKind::While { cond, body } => {
                self.expr(cond);
                self.loop_depth += 1;
                self.block(body);
                self.loop_depth -= 1;
            }
            StmtKind::For { var, iter, body, .. } => {
                match iter {
                    ForIter::Range { lo, hi, .. } => {
                        self.expr(lo);
                        self.expr(hi);
                    }
                    ForIter::Expr(e) => self.expr(e),
                }
                self.scopes.push(HashSet::new());
                self.scopes.last_mut().unwrap().insert(var.clone());
                self.loop_depth += 1;
                self.block(body);
                self.loop_depth -= 1;
                self.scopes.pop();
            }
            StmtKind::Break | StmtKind::Continue => {
                if self.loop_depth == 0 {
                    let w = if matches!(s.kind, StmtKind::Break) { "break" } else { "continue" };
                    self.diags.push(
                        Diagnostic::error(s.span, format!("`{}` can only be used inside a loop.", w))
                            .help("Put it inside a `for` or `while` loop."),
                    );
                }
            }
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    self.expr(e);
                }
            }
            StmtKind::Pass => {}
        }
    }

    fn unknown_name(&mut self, name: &str, span: Span, want_function: bool) {
        let mut cands: Vec<&str> = vec![];
        for sc in &self.scopes {
            cands.extend(sc.iter().map(|s| s.as_str()));
        }
        cands.extend(self.fns[self.cur].iter().map(|s| s.as_str()));
        cands.extend(crate::builtins::BUILTIN_NAMES.iter().copied());
        cands.extend(self.structs.keys().map(|s| s.as_str()));
        let what = if want_function { "function" } else { "variable" };
        let mut d = Diagnostic::error(span, format!("Unknown {} '{}'.", what, name));
        if let Some(sg) = suggest(name, cands.into_iter()) {
            d = d.help(format!("Did you mean '{}'?", sg));
        } else if !want_function {
            d = d.help(format!("Variables are created by assigning to them first, for example `{} = 0`.", name));
        }
        self.diags.push(d);
    }

    fn module_for_alias(&self, alias: &str) -> Option<usize> {
        self.mods[self.cur].imports.iter().find(|(a, _)| a == alias).map(|(_, i)| *i)
    }
    fn plugin_for_alias(&self, alias: &str) -> Option<usize> {
        self.mods[self.cur].plugins.iter().find(|(a, _)| a == alias).map(|(_, i)| *i)
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::None => {}
            ExprKind::Name(n) => {
                if !self.declared(n) {
                    if self.fns[self.cur].contains(n) || is_builtin(n) {
                        self.diags.push(
                            Diagnostic::error(e.span, format!("'{}' is a function, not a value.", n))
                                .help(format!("Call it with parentheses: {}(...)", n)),
                        );
                    } else if self.structs.contains_key(n) {
                        self.diags.push(
                            Diagnostic::error(e.span, format!("'{}' is a struct, not a value.", n))
                                .help(format!("Create one with {}(field=value, ...)", n)),
                        );
                    } else {
                        self.unknown_name(n, e.span, false);
                    }
                }
            }
            ExprKind::Unary(_, a) => self.expr(a),
            ExprKind::Binary(_, a, b) => {
                self.expr(a);
                self.expr(b);
            }
            ExprKind::Call { callee, args } => {
                for a in args {
                    self.expr(&a.value);
                }
                match &callee.kind {
                    ExprKind::Name(n) => {
                        if self.declared(n) {
                            self.diags.push(Diagnostic::error(
                                callee.span,
                                format!("'{}' is a variable, not a function, so it cannot be called.", n),
                            ));
                        } else if !(self.fns[self.cur].contains(n) || is_builtin(n) || self.structs.contains_key(n)) {
                            self.unknown_name(n, callee.span, true);
                        }
                    }
                    ExprKind::Field { base, name, name_span } => {
                        if let ExprKind::Name(m) = &base.kind {
                            if self.declared(m) {
                                // method-call syntax on a variable
                                if !(self.fns[self.cur].contains(name) || is_builtin(name)) {
                                    self.unknown_name(name, *name_span, true);
                                }
                            } else if let Some(mi) = self.module_for_alias(m) {
                                if !self.fns[mi].contains(name) {
                                    let c: Vec<&str> = self.fns[mi].iter().map(|s| s.as_str()).collect();
                                    let mut d = Diagnostic::error(
                                        *name_span,
                                        format!("The module '{}' has no function '{}'.", m, name),
                                    );
                                    if let Some(s) = suggest(name, c.into_iter()) {
                                        d = d.help(format!("Did you mean '{}'?", s));
                                    }
                                    self.diags.push(d);
                                }
                            } else if let Some(pi) = self.plugin_for_alias(m) {
                                if !self.plugins[pi].funcs.iter().any(|f| &f.name == name) {
                                    let c: Vec<&str> = self.plugins[pi].funcs.iter().map(|f| f.name.as_str()).collect();
                                    let mut d = Diagnostic::error(
                                        *name_span,
                                        format!("The plugin '{}' does not export a function '{}'.", m, name),
                                    );
                                    if let Some(s) = suggest(name, c.into_iter()) {
                                        d = d.help(format!("Did you mean '{}'?", s));
                                    }
                                    self.diags.push(d);
                                }
                            } else {
                                self.expr(base);
                            }
                        } else {
                            self.expr(base);
                            if !(self.fns[self.cur].contains(name) || is_builtin(name)) {
                                self.unknown_name(name, *name_span, true);
                            }
                        }
                    }
                    _ => {
                        self.diags.push(Diagnostic::error(callee.span, "Only functions can be called."));
                    }
                }
            }
            ExprKind::Field { base, .. } => self.expr(base),
            ExprKind::Index { base, index } => {
                self.expr(base);
                self.expr(index);
            }
            ExprKind::List(es) => {
                for x in es {
                    self.expr(x);
                }
            }
            ExprKind::Try(a) => self.expr(a),
        }
    }
}
