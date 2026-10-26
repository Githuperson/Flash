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
    /// For instances of functions with inferred parameter types: where it was called from.
    pub call_site: Option<(Span, String)>,
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
    pub enums: Vec<EnumInfo>,
    pub enum_ids: HashMap<String, EnumId>,
    pub fns: Vec<FnItem>,
    pub fn_index: HashMap<(usize, String), usize>,
    pub instances: HashMap<(usize, Vec<Type>), FuncId>,
    pub funcs: Vec<Option<HFunc>>,
    pub ret_of: Vec<Option<Type>>,
    pub stack: Vec<FnCtx>,
    pub plugins: &'a [PluginBinding],
    pub externs: Vec<ExternFn>,
    pub extern_index: HashMap<(usize, String), usize>,
}

pub fn check_program(mods: &[LoadedModule], plugins: &[PluginBinding], diags: &mut Diagnostics) -> Option<Program> {
    let mut c = Checker {
        mods,
        diags,
        structs: vec![],
        struct_ids: HashMap::new(),
        enums: vec![],
        enum_ids: HashMap::new(),
        fns: vec![],
        fn_index: HashMap::new(),
        instances: HashMap::new(),
        funcs: vec![],
        ret_of: vec![],
        stack: vec![],
        plugins,
        externs: vec![],
        extern_index: HashMap::new(),
    };
    c.run()
}

pub fn he(kind: HKind, ty: Type, span: Span) -> HExpr {
    HExpr { kind, ty, span }
}

/// An integer constant expression built only from literals (`7`, `-1`, `~0`, `(1 + 2) * 3`).
/// Such an expression has no type of its own: it takes the integer type of its context.
pub fn is_int_lit(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Int(_) => true,
        ExprKind::Unary(UnOp::Neg | UnOp::BitNot, a) => is_int_lit(a),
        ExprKind::Binary(
            BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Rem
            | BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor
            | BinOp::Shl
            | BinOp::Shr,
            a,
            b,
        ) => is_int_lit(a) && is_int_lit(b),
        _ => false,
    }
}

impl<'a> Checker<'a> {
    pub fn cx(&mut self) -> &mut FnCtx {
        self.stack.last_mut().unwrap()
    }

    pub fn tn(&self, t: &Type) -> String {
        display(t, &self.structs, &self.enums)
    }

    fn annotate(&self, mut d: Diagnostic) -> Diagnostic {
        if let Some(cx) = self.stack.last() {
            if let Some((sp, what)) = &cx.call_site {
                d = d.note(*sp, what.clone());
            }
        }
        d
    }

    pub fn error(&mut self, d: Diagnostic) {
        let d = self.annotate(d);
        self.diags.push(d);
    }

    pub fn fail<T>(&mut self, d: Diagnostic) -> R<T> {
        let d = self.annotate(d);
        self.diags.push(d);
        Err(())
    }

    fn run(&mut self) -> Option<Program> {
        // Structs: register names first (they may refer to each other), then resolve fields.
        for m in self.mods.iter() {
            for it in &m.ast.items {
                if let Item::Enum(e) = it {
                    if !self.enum_ids.contains_key(&e.name) {
                        self.enum_ids.insert(e.name.clone(), self.enums.len());
                        self.enums.push(EnumInfo { name: e.name.clone(), variants: vec![], span: e.name_span, size: 8 });
                    }
                }
                if let Item::Struct(s) = it {
                    if !self.struct_ids.contains_key(&s.name) {
                        self.struct_ids.insert(s.name.clone(), self.structs.len());
                        self.structs.push(StructInfo { name: s.name.clone(), fields: vec![], span: s.name_span, offsets: vec![], size: 8, tuple: false });
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
        for m in self.mods.iter() {
            for it in &m.ast.items {
                if let Item::Enum(e) = it {
                    let id = self.enum_ids[&e.name];
                    let mut variants = vec![];
                    for v in &e.variants {
                        let mut fields = vec![];
                        for f in &v.fields {
                            if let Ok(t) = self.resolve_type(&f.ty) {
                                fields.push((f.name.clone(), t));
                            }
                        }
                        variants.push(VariantInfo { name: v.name.clone(), fields, offsets: vec![] });
                    }
                    self.enums[id].variants = variants;
                }
            }
        }
        for i in 0..self.structs.len() {
            self.structs[i].compute_layout();
        }
        for i in 0..self.enums.len() {
            self.enums[i].compute_layout();
        }
        for (mi, m) in self.mods.iter().enumerate() {
            for it in &m.ast.items {
                if let Item::Fn(f) = it {
                    if let Some(o) = &f.owner {
                        if !self.struct_ids.contains_key(o) && !self.enum_ids.contains_key(o) {
                            self.error(
                                Diagnostic::error(f.name_span, format!("There is no struct called '{}' to attach the method '{}' to.", o, f.name))
                                    .help(format!("Define the struct first (`struct {}:` with its fields), or write a plain function instead.", o)),
                            );
                        }
                    }
                    self.fn_index.insert((mi, f.name.clone()), self.fns.len());
                    self.fns.push(FnItem { def: Rc::new(f.clone()), module: mi });
                }
            }
        }

        // `extern fn` declarations: only ABI-safe types are allowed.
        for (mi, m) in self.mods.iter().enumerate() {
            for it in &m.ast.items {
                if let Item::ExternFn(f) = it {
                    let mut params = vec![];
                    let mut ok = true;
                    for (pn, te) in &f.params {
                        match self.resolve_type(te) {
                            Ok(t) if matches!(t, Type::Int(_) | Type::Float | Type::Bool | Type::Str) => params.push((pn.clone(), t)),
                            Ok(t) => {
                                let tn = self.tn(&t);
                                self.error(Diagnostic::error(te.span, format!("An `extern fn` parameter cannot have type {}.", tn)).help(
                                    "C functions can take integers, Float, Bool and String (passed as a pointer and a byte length).",
                                ));
                                ok = false;
                            }
                            Err(_) => ok = false,
                        }
                    }
                    let ret = match &f.ret {
                        None => Type::None,
                        Some(te) => match self.resolve_type(te) {
                            Ok(t) if matches!(t, Type::Int(_) | Type::Float | Type::Bool | Type::None) => t,
                            Ok(t) => {
                                let tn = self.tn(&t);
                                self.error(
                                    Diagnostic::error(te.span, format!("An `extern fn` cannot return {}.", tn))
                                        .help("C functions can return integers, Float, Bool or nothing. Returning strings across the C boundary needs an ownership convention: use a native plugin (see PLUGIN_ABI.md)."),
                                );
                                ok = false;
                                Type::None
                            }
                            Err(_) => {
                                ok = false;
                                Type::None
                            }
                        },
                    };
                    if ok {
                        self.extern_index.insert((mi, f.name.clone()), self.externs.len());
                        self.externs.push(ExternFn { name: f.name.clone(), params, ret });
                    }
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
            call_site: None,
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
        self.funcs[main_id] =
            Some(HFunc { name: "main".into(), params: vec![], locals: cx.locals, ret: Type::None, body, span, is_main: true });

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
        Some(Program {
            funcs,
            structs: std::mem::take(&mut self.structs),
            enums: std::mem::take(&mut self.enums),
            main: main_id,
            plugins: self.plugins.to_vec(),
            externs: std::mem::take(&mut self.externs),
        })
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
            "Tuple" => {
                let mut elems = vec![];
                for a in &t.args {
                    elems.push(self.resolve_type(a)?);
                }
                if elems.is_empty() {
                    return self.fail(Diagnostic::error(t.span, "A tuple type needs at least one element type.").help("For example: (Int, String)"));
                }
                Ok(Type::Struct(self.tuple_type(elems)))
            }
            n => {
                if let Some(id) = self.struct_ids.get(n) {
                    return self.no_args(t, Type::Struct(*id));
                }
                if let Some(id) = self.enum_ids.get(n) {
                    return self.no_args(t, Type::Enum(*id));
                }
                self.fail(Diagnostic::error(t.span, format!("I don't know a type called '{}'.", n)))
            }
        }
    }

    fn find_variant(&self, eid: EnumId, name: &str) -> Option<usize> {
        self.enums[eid].variants.iter().position(|v| v.name == name)
    }

    /// An unqualified variant name (`Circle`), if exactly one enum has it and nothing else uses the name.
    fn unqualified_variant(&mut self, name: &str) -> Option<(EnumId, usize)> {
        let module = self.cx().module;
        if self.fn_index.contains_key(&(module, name.to_string())) || self.struct_ids.contains_key(name) {
            return None;
        }
        let mut found = None;
        for (eid, e) in self.enums.iter().enumerate() {
            if let Some(vi) = e.variants.iter().position(|v| v.name == name) {
                if found.is_some() {
                    return None; // ambiguous: write Enum.Variant
                }
                found = Some((eid, vi));
            }
        }
        found
    }

    fn construct_variant(&mut self, eid: EnumId, vi: usize, args: &[Arg], span: Span) -> R<HExpr> {
        let fields = self.enums[eid].variants[vi].fields.clone();
        let names: Vec<String> = fields.iter().map(|(n, _)| n.clone()).collect();
        let what = format!("The variant {}.{}", self.enums[eid].name, self.enums[eid].variants[vi].name);
        let list: Vec<(Option<&str>, &Expr)> = args.iter().map(|a| (a.name.as_deref(), &a.value)).collect();
        let bound = self.bind_args(&what, &names, &list, span)?;
        let mut hs = vec![];
        let mut failed = false;
        for ((fname, fty), e) in fields.iter().zip(bound.iter()) {
            match self.expr(e, Some(fty), Ctx::Value) {
                Ok(h) => {
                    if h.ty != *fty && h.ty != Type::Error {
                        let what = format!("The value '{}'", fname);
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
        Ok(he(HKind::Variant { enum_id: eid, variant: vi, fields: hs }, Type::Enum(eid), span))
    }

    /// `match value:` — checked for completeness, then turned into an `if` chain over the variant tag.
    fn match_stmt(&mut self, scrutinee: &Expr, arms: &[MatchArm], span: Span, out: &mut Vec<HStmt>) -> R<()> {
        let hs = self.expr(scrutinee, None, Ctx::Borrow)?;
        let sty = hs.ty.clone();
        if sty == Type::Error {
            return Err(());
        }
        let aliasing = is_place(&hs) && sty.is_owned();
        let root = Self::place_root(&hs);
        let tmp = self.add_local("<match>", sty.clone(), if aliasing { LocalKind::LoopRef } else { LocalKind::Var }, scrutinee.span);
        out.push(HStmt { kind: HStmtKind::Let(tmp, hs), span });
        let tmp_ref = |s: Span, t: &Type| he(HKind::Local(tmp, if t.is_copy() { UseMode::Copy } else { UseMode::Borrow }), t.clone(), s);

        // (condition, body); a missing condition means "always"
        let mut cases: Vec<(Option<HExpr>, Vec<HStmt>)> = vec![];
        let mut covered_variants: Vec<bool> = vec![];
        let mut seen_true = false;
        let mut seen_false = false;
        let mut has_wild = false;
        let mut uses_alias = false;
        if let Type::Enum(eid) = &sty {
            covered_variants = vec![false; self.enums[*eid].variants.len()];
        }
        let mut failed = false;
        for arm in arms {
            if has_wild {
                self.error(Diagnostic::error(arm.span, "This arm can never match because an earlier `_` already matches everything."));
                failed = true;
                continue;
            }
            self.cx().scopes.push(HashMap::new());
            let mut body = vec![];
            let mut cond: Option<HExpr> = None;
            let mut ok = true;
            match &arm.pattern {
                Pattern::Wildcard(_) => has_wild = true,
                Pattern::Lit(le) => {
                    let lit = if matches!(sty, Type::Int(_) | Type::Bool | Type::Str) { self.expr(le, Some(&sty), Ctx::Borrow) } else { Err(()) };
                    match lit {
                        Ok(l) if l.ty == sty => {
                            if let HKind::Bool(b) = l.kind {
                                if b { seen_true = true } else { seen_false = true }
                            }
                            cond = Some(he(HKind::Binary(BinOp::Eq, Box::new(tmp_ref(arm.span, &sty)), Box::new(l)), Type::Bool, arm.span));
                        }
                        Ok(l) => {
                            let (a, b) = (self.tn(&sty), self.tn(&l.ty));
                            self.error(Diagnostic::error(le.span, format!("This pattern is {}, but the value being matched is {}.", b, a)));
                            ok = false;
                        }
                        Err(_) => {
                            if !matches!(sty, Type::Int(_) | Type::Bool | Type::Str) {
                                let tn = self.tn(&sty);
                                self.error(Diagnostic::error(le.span, format!("A literal pattern cannot match a value of type {}.", tn)));
                            }
                            ok = false;
                        }
                    }
                }
                Pattern::Variant { enum_name, name, binds, span: psp } => {
                    let Type::Enum(eid) = &sty else {
                        let tn = self.tn(&sty);
                        self.error(Diagnostic::error(*psp, format!("'{}' is a variant pattern, but the value being matched is {}, not an enum.", name, tn)));
                        self.cx().scopes.pop();
                        failed = true;
                        continue;
                    };
                    let eid = *eid;
                    let ename = self.enums[eid].name.clone();
                    if let Some(en) = enum_name {
                        if *en != ename {
                            self.error(Diagnostic::error(*psp, format!("This pattern is for the enum {}, but the value being matched is {}.", en, ename)));
                            ok = false;
                        }
                    }
                    match if ok { self.find_variant(eid, name) } else { None } {
                        Some(vi) => {
                            if covered_variants[vi] {
                                self.error(Diagnostic::error(*psp, format!("The variant {} is already handled by an earlier arm.", name)));
                                ok = false;
                            }
                            covered_variants[vi] = true;
                            let fields = self.enums[eid].variants[vi].fields.clone();
                            if !binds.is_empty() && binds.len() != fields.len() {
                                self.error(Diagnostic::error(
                                    *psp,
                                    format!("The variant {} has {} value(s), but this pattern names {}.", name, fields.len(), binds.len()),
                                ));
                                ok = false;
                            }
                            let tag = he(HKind::EnumTag(Box::new(tmp_ref(arm.span, &sty))), Type::Int(INT), arm.span);
                            let idx = he(HKind::Int(vi as i64), Type::Int(INT), arm.span);
                            cond = Some(he(HKind::Binary(BinOp::Eq, Box::new(tag), Box::new(idx)), Type::Bool, arm.span));
                            if ok {
                                for (i, b) in binds.iter().enumerate() {
                                    if let Bind::Name(n, bsp) = b {
                                        let fty = fields[i].1.clone();
                                        let src = he(HKind::EnumField(Box::new(tmp_ref(arm.span, &sty)), vi, i), fty.clone(), *bsp);
                                        let kind = if fty.is_copy() {
                                            LocalKind::Var
                                        } else {
                                            uses_alias = true;
                                            LocalKind::LoopRef
                                        };
                                        let id = self.add_local(n, fty, kind, *bsp);
                                        body.push(HStmt { kind: HStmtKind::Let(id, src), span: *bsp });
                                    }
                                }
                            }
                        }
                        None => {
                            if ok {
                                let c: Vec<&str> = self.enums[eid].variants.iter().map(|v| v.name.as_str()).collect();
                                let mut d = Diagnostic::error(*psp, format!("The enum {} has no variant '{}'.", ename, name));
                                if let Some(s) = suggest(name, c.iter().copied()) {
                                    d = d.help(format!("Did you mean '{}'?", s));
                                } else {
                                    d = d.help(format!("Its variants are: {}.", c.join(", ")));
                                }
                                self.error(d);
                            }
                            ok = false;
                        }
                    }
                }
            }
            if ok {
                self.stmts_into(&arm.body, &mut body);
            } else {
                failed = true;
            }
            self.cx().scopes.pop();
            cases.push((cond, body));
        }
        // completeness
        let complete = has_wild
            || match &sty {
                Type::Enum(_) => covered_variants.iter().all(|c| *c),
                Type::Bool => seen_true && seen_false,
                _ => false,
            };
        if !complete && !failed {
            let mut d = Diagnostic::error(span, "This `match` does not cover every possible value.");
            if let Type::Enum(eid) = &sty {
                let missing: Vec<&str> =
                    self.enums[*eid].variants.iter().zip(&covered_variants).filter(|(_, c)| !**c).map(|(v, _)| v.name.as_str()).collect();
                d = d.help(format!("Missing: {}. Add an arm for each, or a final `_:` arm.", missing.join(", ")));
            } else {
                d = d.help("Add a final `_:` arm for all other values.");
            }
            self.error(d);
            failed = true;
        }
        if failed {
            return Err(());
        }
        // Build the if / else-if chain. Without a `_` arm the last arm is the `else`, because the
        // earlier ones leave exactly its case.
        let n = cases.len();
        let mut chain: Vec<HStmt> = vec![];
        for (i, (cond, body)) in cases.into_iter().enumerate().rev() {
            match cond {
                Some(c) if !(i == n - 1 && !has_wild) => {
                    chain = vec![HStmt { kind: HStmtKind::If(c, body, chain), span }];
                }
                _ => chain = body,
            }
        }
        match (uses_alias, root) {
            (true, Some(root)) if aliasing => out.push(HStmt { kind: HStmtKind::Borrowed { root, body: chain }, span }),
            _ => out.extend(chain),
        }
        Ok(())
    }

    fn mk_copy(&self, e: HExpr, span: Span) -> HExpr {
        let ty = e.ty.clone();
        he(HKind::Builtin(Builtin::Copy, vec![e]), ty, span)
    }

    /// The (anonymous) struct that represents the tuple type with these element types.
    pub fn tuple_type(&mut self, elems: Vec<Type>) -> StructId {
        let inner: Vec<String> = elems.iter().map(|t| self.tn(t)).collect();
        let name = if inner.len() == 1 { format!("({},)", inner[0]) } else { format!("({})", inner.join(", ")) };
        if let Some(&id) = self.struct_ids.get(&name) {
            return id;
        }
        let fields: Vec<(String, Type)> = elems.into_iter().enumerate().map(|(i, t)| (i.to_string(), t)).collect();
        let mut info = StructInfo { name: name.clone(), fields, span: Span::default(), offsets: vec![], size: 8, tuple: true };
        info.compute_layout();
        let id = self.structs.len();
        self.structs.push(info);
        self.struct_ids.insert(name, id);
        id
    }

    fn tuple_lit(&mut self, elems: &[Expr], expected: Option<&Type>, span: Span) -> R<HExpr> {
        let hint: Option<Vec<Type>> = match expected {
            Some(Type::Struct(sid)) if self.structs[*sid].tuple && self.structs[*sid].fields.len() == elems.len() => {
                Some(self.structs[*sid].fields.iter().map(|(_, t)| t.clone()).collect())
            }
            _ => None,
        };
        let mut hs = vec![];
        let mut failed = false;
        for (i, e) in elems.iter().enumerate() {
            let h = self.expr(e, hint.as_ref().map(|h| &h[i]), Ctx::Value);
            match h {
                Ok(h) => hs.push(h),
                Err(_) => failed = true,
            }
        }
        if failed {
            return Err(());
        }
        let tys: Vec<Type> = hs.iter().map(|h| h.ty.clone()).collect();
        let sid = self.tuple_type(tys);
        Ok(he(HKind::Struct(hs), Type::Struct(sid), span))
    }

    /// `a, b = value`: evaluate the tuple once, then copy each element into its target.
    fn unpack(&mut self, targets: &[Expr], value: &Expr, span: Span, out: &mut Vec<HStmt>) -> R<()> {
        let v = self.expr(value, None, Ctx::Value)?;
        let sid = match &v.ty {
            Type::Struct(sid) if self.structs[*sid].tuple => *sid,
            Type::Error => return Err(()),
            other => {
                let tn = self.tn(other);
                return self.fail(
                    Diagnostic::error(value.span, format!("Only a tuple can be unpacked into several names, but this is {}.", tn))
                        .help("Return several values with `return a, b`, then write `x, y = f()`."),
                );
            }
        };
        let fields = self.structs[sid].fields.clone();
        if fields.len() != targets.len() {
            let tn = self.tn(&v.ty);
            return self.fail(Diagnostic::error(
                span,
                format!("This is a tuple of {} values ({}), but {} names are being assigned.", fields.len(), tn, targets.len()),
            ));
        }
        let tmp = self.add_local("<tuple>", v.ty.clone(), LocalKind::Var, span);
        out.push(HStmt { kind: HStmtKind::Let(tmp, v), span });
        for (i, target) in targets.iter().enumerate() {
            let fty = fields[i].1.clone();
            let base = he(HKind::Local(tmp, UseMode::Borrow), Type::Struct(sid), span);
            let field = he(HKind::Field(Box::new(base), i), fty.clone(), span);
            let elem = if fty.is_copy() { field } else { self.mk_copy(field, span) };
            match &target.kind {
                ExprKind::Name(name) => {
                    if let Some(id) = self.lookup(name) {
                        let (lty, lkind) = {
                            let l = &self.cx().locals[id];
                            (l.ty.clone(), l.kind)
                        };
                        if lty != fty {
                            let (a, b) = (self.tn(&lty), self.tn(&fty));
                            return self.fail(Diagnostic::error(target.span, format!("'{}' is {}, but this element is {}.", name, a, b)));
                        }
                        if lkind != LocalKind::Var {
                            return self.fail(Diagnostic::error(target.span, format!("Cannot assign to '{}' here (it is a parameter or loop variable).", name)));
                        }
                        let place = he(HKind::Local(id, UseMode::Borrow), lty, target.span);
                        out.push(HStmt { kind: HStmtKind::Assign(place, elem), span });
                    } else {
                        let id = self.add_local(name, fty, LocalKind::Var, target.span);
                        out.push(HStmt { kind: HStmtKind::Let(id, elem), span });
                    }
                }
                ExprKind::Field { .. } | ExprKind::Index { .. } => {
                    let place = self.expr(target, None, Ctx::Borrow)?;
                    self.check_mut_place(&place, target.span, "assign to")?;
                    if place.ty != fty {
                        return self.type_mismatch(target.span, &place.ty.clone(), &elem, "this place");
                    }
                    out.push(HStmt { kind: HStmtKind::Assign(place, elem), span });
                }
                _ => {
                    return self.fail(Diagnostic::error(target.span, "You can only unpack into variables, fields (`a.b`) or list elements (`a[i]`)."));
                }
            }
        }
        Ok(())
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
            call_site: if def.params.iter().any(|p| p.ty.is_none()) {
                let tys: Vec<String> = arg_tys.iter().map(|t| self.tn(t)).collect();
                Some((call_span, format!("while checking '{}' with argument types ({}), called here", def.name, tys.join(", "))))
            } else {
                None
            },
        });
        let mut params = vec![];
        for (p, ty) in def.params.iter().zip(arg_tys.iter()) {
            let kind = match p.mode {
                ParamMode::Own => LocalKind::ParamOwn,
                ParamMode::Mut => {
                    if ty.is_copy() {
                        self.error(
                            Diagnostic::error(
                                p.span,
                                format!("A `mut` parameter must be a String, List or struct, but '{}' is {}.", p.name, self.tn(ty)),
                            )
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
                Diagnostic::error(def.name_span, format!("The function '{}' can reach its end without returning a value.", def.name)).help(
                    format!(
                        "Every path through the function must `return` a {} (add a `return` at the end or in the `else` branch).",
                        self.tn(&ret)
                    ),
                ),
            );
        }
        self.funcs[id] = Some(HFunc { name: def.name.clone(), params, locals: cx.locals, ret, body, span: def.span, is_main: false });
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
            StmtKind::Unpack { targets, value } => self.unpack(targets, value, span, out)?,
            StmtKind::Match { scrutinee, arms } => self.match_stmt(scrutinee, arms, span, out)?,
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
                Diagnostic::error(span, "`return` can only be used inside a function.").help("To stop the program early, call exit(0)."),
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
                        let what = if lkind == LocalKind::LoopRef {
                            "a loop variable that borrows a list element"
                        } else {
                            "a read-only parameter"
                        };
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
                return self.fail(Diagnostic::error(
                    target.span,
                    "You can only assign to a variable, a field (`a.b`) or a list element (`a[i]`).",
                ));
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
                    return self.fail(Diagnostic::error(span, format!("The number {} does not fit in {}.", v, ty.name())).help(format!(
                        "{} can hold {} to {}.",
                        ty.name(),
                        ty.min(),
                        ty.max()
                    )));
                }
                Ok(he(HKind::Int(*v as i64), Type::Int(ty), span))
            }
            ExprKind::Float(v) => Ok(he(HKind::Float(*v), Type::Float, span)),
            ExprKind::Str(s) => Ok(he(HKind::Str(s.clone()), Type::Str, span)),
            ExprKind::Bool(b) => Ok(he(HKind::Bool(*b), Type::Bool, span)),
            ExprKind::None => Ok(he(HKind::None, Type::None, span)),
            ExprKind::Name(n) => {
                let Some(id) = self.lookup(n) else {
                    if let Some((eid, vi)) = self.unqualified_variant(n) {
                        return self.construct_variant(eid, vi, &[], span);
                    }
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
                if let ExprKind::Name(en) = &base.kind {
                    if self.lookup(en).is_none() {
                        if let Some(&eid) = self.enum_ids.get(en) {
                            return match self.find_variant(eid, name) {
                                Some(vi) => self.construct_variant(eid, vi, &[], span),
                                None => self.fail(Diagnostic::error(*name_span, format!("The enum {} has no variant '{}'.", en, name))),
                            };
                        }
                    }
                }
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
            ExprKind::Tuple(elems) => self.tuple_lit(elems, expected, span),
            ExprKind::Call { callee, args } => self.call(callee, args, span, expected, ctx),
        }
    }

    /// Moving an owned value out of a field / list slot would leave a hole behind.
    fn check_value_use(&mut self, ty: &Type, ctx: Ctx, span: Span, what: &str) -> R<()> {
        if ctx == Ctx::Value && ty.is_owned() {
            return self.fail(Diagnostic::error(span, format!("Cannot move {} out of its container.", what)).help(
                "Use `copy(...)` to make an independent copy, or read it where a borrow is enough (for example inside `say(...)`).",
            ));
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
            return self.fail(Diagnostic::error(span, format!("The `?` operator needs a Result, but this is {}.", tn)).help(
                "`?` unwraps the value of a fallible call such as read_file(path), or returns its error from the current function.",
            ));
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
            let mut d = Diagnostic::error(b.span, msg).label(if lt.is_numeric() {
                "expected a compatible numeric value".to_string()
            } else {
                format!("expected {}", ln)
            });
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
                if let Some(&ei) = self.extern_index.get(&(module, n.clone())) {
                    return self.call_extern(ei, args, span);
                }
                if let Some(&sid) = self.struct_ids.get(n) {
                    return self.construct(sid, args, span);
                }
                if let Some((eid, vi)) = self.unqualified_variant(n) {
                    return self.construct_variant(eid, vi, args, span);
                }
                if is_builtin(n) {
                    let list: Vec<&Expr> = args.iter().map(|a| &a.value).collect();
                    if let Some(a) = args.iter().find(|a| a.name.is_some()) {
                        return self.fail(Diagnostic::error(
                            a.value.span,
                            format!("The built-in function '{}' does not take named arguments.", n),
                        ));
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
                            if let Some(&ei) = self.extern_index.get(&(mi, name.clone())) {
                                return self.call_extern(ei, args, span);
                            }
                            return Err(());
                        }
                        if let Some((_, pi)) = self.mods[module].plugins.iter().find(|(a, _)| a == m) {
                            let pi = *pi;
                            return self.call_plugin(pi, name, args, span);
                        }
                    }
                }
                // Static method call: Player.new(args)
                if let ExprKind::Name(tn) = &base.kind {
                    if self.lookup(tn).is_none() && (self.struct_ids.contains_key(tn) || self.enum_ids.contains_key(tn)) {
                        // Shape.Circle(2.0): an enum variant
                        if let Some(&eid) = self.enum_ids.get(tn) {
                            if let Some(vi) = self.find_variant(eid, name) {
                                return self.construct_variant(eid, vi, args, span);
                            }
                        }
                        let Some(fidx) = self.find_method(tn, name) else {
                            return self.fail(Diagnostic::error(callee.span, format!("The struct {} has no method '{}'.", tn, name)).help(format!(
                                "Define it with `fn {}.{}(...):`.",
                                tn, name
                            )));
                        };
                        let list: Vec<(Option<&str>, &Expr)> = args.iter().map(|a| (a.name.as_deref(), &a.value)).collect();
                        return self.call_user(fidx, &list, span);
                    }
                }
                // Method call on a value: p.level_up(args) == Player.level_up(p, args)
                let owner: Option<String> = match self.static_type(base) {
                    Some(Type::Struct(sid)) => Some(self.structs[sid].name.clone()),
                    Some(Type::Enum(eid)) => Some(self.enums[eid].name.clone()),
                    _ => None,
                };
                let cur_module = self.cx().module;
                let method = match &owner {
                    Some(o) => self.find_method(o, name),
                    None if !is_builtin(name) && !self.fn_index.contains_key(&(cur_module, name.clone())) => self.unique_method_named(name),
                    None => None,
                };
                if let Some(fidx) = method {
                    let mut list: Vec<(Option<&str>, &Expr)> = vec![(None, &**base)];
                    list.extend(args.iter().map(|a| (a.name.as_deref(), &a.value)));
                    return self.call_user(fidx, &list, span);
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

    /// The type of a simple expression (variable, field, element) without checking it, or None.
    fn static_type(&mut self, e: &Expr) -> Option<Type> {
        match &e.kind {
            ExprKind::Name(n) => {
                let id = self.lookup(n)?;
                Some(self.cx().locals[id].ty.clone())
            }
            ExprKind::Field { base, name, .. } => match self.static_type(base)? {
                Type::Struct(sid) => self.structs[sid].fields.iter().find(|(f, _)| f == name).map(|(_, t)| t.clone()),
                _ => None,
            },
            ExprKind::Index { base, .. } => match self.static_type(base)? {
                Type::List(el) => Some(*el),
                _ => None,
            },
            _ => None,
        }
    }

    /// The function index of method `name` of struct `owner`, defined in this module or any other.
    fn find_method(&mut self, owner: &str, name: &str) -> Option<usize> {
        let key = format!("{}.{}", owner, name);
        let module = self.cx().module;
        if let Some(&f) = self.fn_index.get(&(module, key.clone())) {
            return Some(f);
        }
        let mut found: Vec<usize> = self.fn_index.iter().filter(|((_, k), _)| *k == key).map(|(_, f)| *f).collect();
        found.sort();
        found.first().copied()
    }

    /// When the receiver's type is not known without checking it: the method called `name` if
    /// exactly one struct has one.
    fn unique_method_named(&mut self, name: &str) -> Option<usize> {
        let suffix = format!(".{}", name);
        let mut found: Vec<usize> = self
            .fns
            .iter()
            .enumerate()
            .filter(|(_, f)| f.def.owner.is_some() && f.def.name.ends_with(&suffix))
            .map(|(i, _)| i)
            .collect();
        found.dedup();
        if found.len() == 1 {
            Some(found[0])
        } else {
            None
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
                            format!(
                                "{} takes {} argument{}, but more were given.",
                                what,
                                names.len(),
                                if names.len() == 1 { "" } else { "s" }
                            ),
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
                    self.error(Diagnostic::error(
                        e.span,
                        format!("A `mut` parameter must be a String, List or struct, but this is {}.", tn),
                    ));
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
                Diagnostic::error(
                    span,
                    format!(
                        "I can't work out what '{}' returns because it calls itself before any `return` that decides its type.",
                        def.name
                    ),
                )
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

    fn call_extern(&mut self, ei: usize, args: &[Arg], span: Span) -> R<HExpr> {
        let f = self.externs[ei].clone();
        let what = format!("The extern function '{}'", f.name);
        if let Some(a) = args.iter().find(|a| a.name.is_some()) {
            return self.fail(Diagnostic::error(a.value.span, format!("{} does not take named arguments.", what)));
        }
        if args.len() != f.params.len() {
            return self.fail(Diagnostic::error(
                span,
                format!(
                    "{} takes {} argument{}, but {} {} given.",
                    what,
                    f.params.len(),
                    if f.params.len() == 1 { "" } else { "s" },
                    args.len(),
                    if args.len() == 1 { "was" } else { "were" }
                ),
            ));
        }
        let mut hs = vec![];
        let mut failed = false;
        for (a, (pn, pt)) in args.iter().zip(f.params.iter()) {
            match self.expr(&a.value, Some(pt), Ctx::Borrow) {
                Ok(h) => {
                    if h.ty != *pt && h.ty != Type::Error {
                        let what = format!("The argument '{}' of '{}'", pn, f.name);
                        let _ = self.type_mismatch::<()>(a.value.span, pt, &h, &what);
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
        Ok(he(HKind::ExternCall { index: ei, args: hs }, f.ret.clone(), span))
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
                format!(
                    "{} takes {} argument{}, but {} {} given.",
                    what,
                    f.params.len(),
                    if f.params.len() == 1 { "" } else { "s" },
                    args.len(),
                    if args.len() == 1 { "was" } else { "were" }
                ),
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
        HStmtKind::Borrowed { body, .. } => contains_break(body),
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
            HStmtKind::Borrowed { body, .. } => always_returns(body),
            HStmtKind::While(c, body) => matches!(c.kind, HKind::Bool(true)) && !contains_break(body),
            _ => false,
        }),
    }
}
