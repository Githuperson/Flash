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
    /// A lambda body: names from the enclosing function are captured (copied in).
    pub lambda: bool,
    /// (local of the enclosing function, its copy in this lambda)
    pub captures: Vec<(LocalId, LocalId)>,
    /// A lambda passed straight to `spawn`: it takes ownership of the strings, lists, ... it uses.
    pub moving: bool,
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
    /// generic struct / enum templates, instantiated on demand as `Name[Args]`
    pub generic_structs: HashMap<String, Rc<StructDef>>,
    pub generic_enums: HashMap<String, Rc<EnumDef>>,
    /// type parameters in scope while a generic's fields are resolved
    pub tsubst: HashMap<String, Type>,
    pub closures: Vec<ClosureTarget>,
    /// set by `spawn` for the lambda it is about to check
    pub move_lambda: bool,
    /// function -> closure target of the wrapper that lets it be used as a value
    pub fn_values: HashMap<FuncId, usize>,
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
        generic_structs: HashMap::new(),
        generic_enums: HashMap::new(),
        tsubst: HashMap::new(),
        closures: vec![],
        move_lambda: false,
        fn_values: HashMap::new(),
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
                    if !e.params.is_empty() {
                        self.generic_enums.entry(e.name.clone()).or_insert_with(|| Rc::new(e.clone()));
                    } else if !self.enum_ids.contains_key(&e.name) {
                        self.enum_ids.insert(e.name.clone(), self.enums.len());
                        self.enums.push(EnumInfo { name: e.name.clone(), variants: vec![], span: e.name_span, size: 8 });
                    }
                }
                if let Item::Struct(s) = it {
                    if !s.params.is_empty() {
                        self.generic_structs.entry(s.name.clone()).or_insert_with(|| Rc::new(s.clone()));
                    } else if !self.struct_ids.contains_key(&s.name) {
                        self.struct_ids.insert(s.name.clone(), self.structs.len());
                        self.structs.push(StructInfo { name: s.name.clone(), fields: vec![], span: s.name_span, offsets: vec![], size: 8, tuple: false });
                    }
                }
            }
        }
        for m in self.mods.iter() {
            for it in &m.ast.items {
                if let Item::Struct(s) = it {
                    if !s.params.is_empty() {
                        continue;
                    }
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
                    if !e.params.is_empty() {
                        continue;
                    }
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
                        if !self.struct_ids.contains_key(o) && !self.enum_ids.contains_key(o) && !self.generic_structs.contains_key(o) && !self.generic_enums.contains_key(o) {
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
            lambda: false,
            captures: vec![],
            moving: false,
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
            if def.params.iter().all(|p| p.ty.is_some() && !self.is_generic_self(p)) {
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
            closures: std::mem::take(&mut self.closures),
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
            "Map" | "Set" => {
                let want = if t.name == "Map" { 2 } else { 1 };
                if t.args.len() != want {
                    let example = if want == 2 { "Map[String, Int]" } else { "Set[Int]" };
                    return self.fail(
                        Diagnostic::error(t.span, format!("{} needs {} type argument(s).", t.name, want)).help(format!("For example: {}", example)),
                    );
                }
                let key = self.resolve_type(&t.args[0])?;
                if !matches!(key, Type::Int(_) | Type::Bool | Type::Str) {
                    let tn = self.tn(&key);
                    return self.fail(
                        Diagnostic::error(t.args[0].span, format!("{} keys must be integers, Bool or String, but this is {}.", t.name, tn))
                            .help("Keys are compared and hashed by value; use an Int id or a String name as the key."),
                    );
                }
                if want == 2 {
                    let val = self.resolve_type(&t.args[1])?;
                    Ok(Type::Map(Box::new(key), Box::new(val)))
                } else {
                    Ok(Type::Set(Box::new(key)))
                }
            }
            "Thread" => self.no_args(t, Type::Thread),
            "Channel" => {
                if t.args.len() != 1 {
                    return self.fail(Diagnostic::error(t.span, "Channel needs one type argument.").help("For example: Channel[Int]"));
                }
                let inner = self.resolve_type(&t.args[0])?;
                Ok(Type::Chan(Box::new(inner)))
            }
            "Fn" => {
                let mut tys = vec![];
                for a in &t.args {
                    tys.push(self.resolve_type(a)?);
                }
                let ret = tys.pop().unwrap_or(Type::None);
                Ok(Type::Func(tys, Box::new(ret)))
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
                if let Some(ty) = self.tsubst.get(n).cloned() {
                    return self.no_args(t, ty);
                }
                if self.generic_structs.contains_key(n) || self.generic_enums.contains_key(n) {
                    let mut args = vec![];
                    for a in &t.args {
                        args.push(self.resolve_type(a)?);
                    }
                    return self.instantiate_generic(n, args, t.span);
                }
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

    fn mk_builtin(&self, b: Builtin, args: Vec<HExpr>, ty: Type, span: Span) -> HExpr {
        he(HKind::Builtin(b, args), ty, span)
    }

    fn map_lit(&mut self, pairs: &[(Expr, Expr)], expected: Option<&Type>, span: Span) -> R<HExpr> {
        if pairs.is_empty() {
            return match expected {
                Some(t @ Type::Map(..)) => Ok(he(HKind::MapLit(vec![]), t.clone(), span)),
                Some(t @ Type::Set(_)) => Ok(he(HKind::SetLit(vec![]), t.clone(), span)),
                _ => self.fail(
                    Diagnostic::error(span, "I can't tell what kind of collection an empty `{}` is.")
                        .help("Give it a type: `m: Map[String, Int] = {}` or `s: Set[Int] = {}`."),
                ),
            };
        }
        let hint = match expected {
            Some(Type::Map(k, v)) => Some(((**k).clone(), (**v).clone())),
            _ => None,
        };
        let mut hs: Vec<(HExpr, HExpr)> = vec![];
        let mut failed = false;
        for (ke, ve) in pairs {
            let (kh, vh) = match (&hint, hs.first()) {
                (Some((k, v)), _) => (Some(k.clone()), Some(v.clone())),
                (None, Some((k0, v0))) => (Some(k0.ty.clone()), Some(v0.ty.clone())),
                _ => (None, None),
            };
            let k = self.expr(ke, kh.as_ref(), Ctx::Borrow);
            let v = self.expr(ve, vh.as_ref(), Ctx::Value);
            match (k, v) {
                (Ok(k), Ok(v)) => {
                    if let Some(kt) = &kh {
                        if k.ty != *kt && k.ty != Type::Error {
                            let _ = self.type_mismatch::<()>(ke.span, kt, &k, "This map key");
                            failed = true;
                            continue;
                        }
                    }
                    if let Some(vt) = &vh {
                        if v.ty != *vt && v.ty != Type::Error {
                            let _ = self.type_mismatch::<()>(ve.span, vt, &v, "This map value");
                            failed = true;
                            continue;
                        }
                    }
                    if !matches!(k.ty, Type::Int(_) | Type::Bool | Type::Str) {
                        let tn = self.tn(&k.ty);
                        self.error(Diagnostic::error(ke.span, format!("Map keys must be integers, Bool or String, but this is {}.", tn)));
                        failed = true;
                        continue;
                    }
                    hs.push((k, v));
                }
                _ => failed = true,
            }
        }
        if failed {
            return Err(());
        }
        let ty = Type::Map(Box::new(hs[0].0.ty.clone()), Box::new(hs[0].1.ty.clone()));
        Ok(he(HKind::MapLit(hs), ty, span))
    }

    fn set_lit(&mut self, elems: &[Expr], expected: Option<&Type>, span: Span) -> R<HExpr> {
        let hint = match expected {
            Some(Type::Set(t)) => Some((**t).clone()),
            _ => None,
        };
        let mut hs: Vec<HExpr> = vec![];
        let mut failed = false;
        for e in elems {
            let want = hint.clone().or_else(|| hs.first().map(|h| h.ty.clone()));
            match self.expr(e, want.as_ref(), Ctx::Borrow) {
                Ok(h) => {
                    if let Some(w) = &want {
                        if h.ty != *w && h.ty != Type::Error {
                            let _ = self.type_mismatch::<()>(e.span, w, &h, "This set element");
                            failed = true;
                            continue;
                        }
                    }
                    if !matches!(h.ty, Type::Int(_) | Type::Bool | Type::Str) {
                        let tn = self.tn(&h.ty);
                        self.error(Diagnostic::error(e.span, format!("Set elements must be integers, Bool or String, but this is {}.", tn)));
                        failed = true;
                        continue;
                    }
                    hs.push(h);
                }
                Err(_) => failed = true,
            }
        }
        if failed {
            return Err(());
        }
        let ty = Type::Set(Box::new(hs[0].ty.clone()));
        Ok(he(HKind::SetLit(hs), ty, span))
    }

    /// `m.has(k)`, `m.get(k, default)`, `m.remove(k)`, `m.keys()`, `m.values()`, `m.clear()`,
    /// `s.add(x)`, `s.has(x)`, `s.remove(x)`, `s.items()`, `s.clear()`.
    fn collection_method(&mut self, base: &Expr, name: &str, args: &[Arg], span: Span) -> R<HExpr> {
        let hb = self.expr(base, None, Ctx::Borrow)?;
        let (is_map, key_ty, val_ty) = match &hb.ty {
            Type::Map(k, v) => (true, (**k).clone(), Some((**v).clone())),
            Type::Set(t) => (false, (**t).clone(), None),
            Type::Error => return Err(()),
            other => {
                let tn = self.tn(other);
                return self.fail(Diagnostic::error(span, format!("{} has no method '{}'.", tn, name)));
            }
        };
        let kind = if is_map { "Map" } else { "Set" };
        let want_args: usize = match (is_map, name) {
            (_, "has") | (_, "remove") | (false, "add") => 1,
            (true, "get") => 2,
            (_, "keys") | (_, "values") | (_, "items") | (_, "clear") => 0,
            _ => {
                let all = if is_map { "has, get, remove, keys, values, clear" } else { "add, has, remove, items, clear" };
                return self.fail(Diagnostic::error(span, format!("{} has no method '{}'.", kind, name)).help(format!("{} methods: {}.", kind, all)));
            }
        };
        if (is_map && name == "items") || (!is_map && (name == "keys" || name == "values")) {
            return self.fail(Diagnostic::error(span, format!("{} has no method '{}'.", kind, name)));
        }
        if args.len() != want_args {
            return self.fail(Diagnostic::error(span, format!("{}.{}() takes {} argument(s), but {} were given.", kind, name, want_args, args.len())));
        }
        if args.iter().any(|a| a.name.is_some()) {
            return self.fail(Diagnostic::error(span, format!("{}.{}() does not take named arguments.", kind, name)));
        }
        let mutating = matches!(name, "add" | "remove" | "clear");
        if mutating {
            self.check_mut_place(&hb, base.span, "change")?;
        }
        let mut hargs = vec![hb];
        if want_args >= 1 {
            let k = self.expr(&args[0].value, Some(&key_ty), Ctx::Borrow)?;
            if k.ty != key_ty && k.ty != Type::Error {
                return self.type_mismatch(args[0].value.span, &key_ty, &k, "The key");
            }
            hargs.push(k);
        }
        let (b, ty) = match (is_map, name) {
            (true, "has") => (Builtin::MapHas, Type::Bool),
            (true, "get") => {
                let v = val_ty.clone().unwrap();
                let ctx = if v.is_owned() { Ctx::Value } else { Ctx::Borrow };
                let d = self.expr(&args[1].value, Some(&v), ctx)?;
                if d.ty != v && d.ty != Type::Error {
                    return self.type_mismatch(args[1].value.span, &v, &d, "The default value");
                }
                hargs.push(d);
                (Builtin::MapGet, v)
            }
            (true, "remove") => (Builtin::MapRemove, Type::None),
            (true, "keys") => (Builtin::MapKeys, Type::List(Box::new(key_ty.clone()))),
            (true, "values") => (Builtin::MapValues, Type::List(Box::new(val_ty.clone().unwrap()))),
            (true, "clear") => (Builtin::MapClear, Type::None),
            (false, "add") => (Builtin::SetAdd, Type::None),
            (false, "has") => (Builtin::SetHas, Type::Bool),
            (false, "remove") => (Builtin::SetRemove, Type::None),
            (false, "items") => (Builtin::SetItems, Type::List(Box::new(key_ty.clone()))),
            (false, "clear") => (Builtin::SetClear, Type::None),
            _ => unreachable!(),
        };
        Ok(self.mk_builtin(b, hargs, ty, span))
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
            if e.name.contains('[') {
                continue; // instances of generic enums are found through `generic_variant`
            }
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
            lambda: false,
            captures: vec![],
            moving: false,
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
        let depth = self.stack.len() - 1;
        self.lookup_in(depth, name)
    }

    /// Find `name` in the function at `depth` of the stack. A lambda that mentions a variable of
    /// the function around it gets its own copy (a "capture"), which only works for plain values.
    fn lookup_in(&mut self, depth: usize, name: &str) -> Option<LocalId> {
        if let Some(id) = self.stack[depth].scopes.iter().rev().find_map(|s| s.get(name).copied()) {
            return Some(id);
        }
        if !self.stack[depth].lambda || depth == 0 {
            return None;
        }
        let outer = self.lookup_in(depth - 1, name)?;
        let (ty, span) = {
            let l = &self.stack[depth - 1].locals[outer];
            (l.ty.clone(), l.span)
        };
        if !ty.is_copy() && !self.stack[depth].moving {
            let tn = self.tn(&ty);
            self.error(
                Diagnostic::error(span, format!("A lambda can only use numbers and Bool from outside itself, but '{}' is {}.", name, tn)).help(
                    format!("Pass '{}' to the lambda as a parameter instead, or use a named function.", name),
                ),
            );
            return None;
        }
        let cx = &mut self.stack[depth];
        cx.locals.push(LocalInfo { name: name.to_string(), ty, kind: LocalKind::Var, span });
        let inner = cx.locals.len() - 1;
        cx.scopes[0].insert(name.to_string(), inner);
        cx.captures.push((outer, inner));
        Some(inner)
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
                // Looping over a Channel receives messages until it is closed and empty.
                if let Type::Chan(t) = list.ty.clone() {
                    return self.for_channel(var, var_span, list, *t, body, span, out);
                }
                // Looping over a Map visits its keys, over a Set its items (in insertion order).
                match list.ty.clone() {
                    Type::Map(k, _) => list = self.mk_builtin(Builtin::MapKeys, vec![list], Type::List(k), e.span),
                    Type::Set(t) => list = self.mk_builtin(Builtin::SetItems, vec![list], Type::List(t), e.span),
                    _ => {}
                }
                let elem = match &list.ty {
                    Type::List(t) => (**t).clone(),
                    Type::Error => return Err(()),
                    other => {
                        let tn = self.tn(other);
                        return self.fail(
                            Diagnostic::error(e.span, format!("You can only loop over a range, a List, a Map or a Set, but this is {}.", tn))
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

    /// `for x in channel:` becomes a loop that receives, stops at the "closed" error and binds x.
    fn for_channel(&mut self, var: &str, var_span: Span, mut chan: HExpr, elem: Type, body: &[Stmt], span: Span, out: &mut Vec<HStmt>) -> R<()> {
        if !is_place(&chan) {
            let tmp = self.add_local("<iter>", chan.ty.clone(), LocalKind::Var, span);
            let cty = chan.ty.clone();
            out.push(HStmt { kind: HStmtKind::Let(tmp, chan), span });
            chan = he(HKind::Local(tmp, UseMode::Borrow), cty, span);
        }
        let rty = Type::Result(Box::new(elem.clone()));
        self.cx().scopes.push(HashMap::new());
        let r = self.add_local("<received>", rty.clone(), LocalKind::Var, span);
        let mut stmts = vec![HStmt { kind: HStmtKind::Let(r, self.mk_builtin(Builtin::ChanRecv, vec![chan], rty.clone(), span)), span }];
        let local_r = |mode| he(HKind::Local(r, mode), rty.clone(), span);
        let ok = self.mk_builtin(Builtin::IsOk, vec![local_r(UseMode::Borrow)], Type::Bool, span);
        let not_ok = he(HKind::Unary(crate::ast::UnOp::Not, Box::new(ok)), Type::Bool, span);
        stmts.push(HStmt { kind: HStmtKind::If(not_ok, vec![HStmt { kind: HStmtKind::Break, span }], vec![]), span });
        let v = self.add_local(var, elem.clone(), LocalKind::Var, var_span);
        let value = self.mk_builtin(Builtin::Unwrap, vec![local_r(UseMode::Move)], elem, span);
        stmts.push(HStmt { kind: HStmtKind::Let(v, value), span });
        self.cx().loop_depth += 1;
        let b = self.block(body);
        self.cx().loop_depth -= 1;
        self.cx().scopes.pop();
        stmts.extend(b);
        out.push(HStmt { kind: HStmtKind::While(he(HKind::Bool(true), Type::Bool, span), stmts), span });
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
                    if let Some((en, vi)) = self.generic_variant(n) {
                        return self.construct_generic_variant(&en, vi, &[], span, expected);
                    }
                    let module = self.cx().module;
                    if let Some(&fidx) = self.fn_index.get(&(module, n.clone())) {
                        return self.fn_value(fidx, expected, span);
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
                        if let Some(ge) = self.generic_enums.get(en).cloned() {
                            return match ge.variants.iter().position(|v| v.name == *name) {
                                Some(vi) => self.construct_generic_variant(en, vi, &[], span, expected),
                                None => self.fail(Diagnostic::error(*name_span, format!("The enum {} has no variant '{}'.", en, name))),
                            };
                        }
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
                if let Type::Map(k, v) = hb.ty.clone() {
                    let hi = self.expr(index, Some(&k), Ctx::Borrow)?;
                    if hi.ty != *k && hi.ty != Type::Error {
                        return self.type_mismatch(index.span, &k, &hi, "The map key");
                    }
                    self.check_value_use(&v, ctx, span, "this map value")?;
                    return Ok(he(HKind::Index(Box::new(hb), Box::new(hi)), *v, span));
                }
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
            ExprKind::Map(pairs) => self.map_lit(pairs, expected, span),
            ExprKind::Set(elems) => self.set_lit(elems, expected, span),
            ExprKind::Lambda { params, body } => self.lambda(params, body, expected, span),
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
                if let Some(id) = self.lookup(n) {
                    return self.call_value(id, n, args, span);
                }
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
                if self.generic_structs.contains_key(n) {
                    return self.construct_generic_struct(n, args, span, expected);
                }
                if let Some((eid, vi)) = self.unqualified_variant(n) {
                    return self.construct_variant(eid, vi, args, span);
                }
                if let Some((en, vi)) = self.generic_variant(n) {
                    return self.construct_generic_variant(&en, vi, args, span, expected);
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
                // A function stored in a struct field: op.run(1, 2)
                if let Some(Type::Struct(sid)) = self.static_type(base) {
                    if self.structs[sid].fields.iter().any(|(f, t)| f == name && matches!(t, Type::Func(..))) {
                        let hb = self.expr(base, None, Ctx::Borrow)?;
                        let idx = self.structs[sid].fields.iter().position(|(f, _)| f == name).unwrap();
                        let fty = self.structs[sid].fields[idx].1.clone();
                        let f = he(HKind::Field(Box::new(hb), idx), fty, span);
                        return self.call_fn_expr(f, name, args, span);
                    }
                }
                // Map / Set methods: m.has(k), s.add(x), ...
                if matches!(self.static_type(base), Some(Type::Map(..) | Type::Set(_))) {
                    return self.collection_method(base, name, args, span);
                }
                // Static method call: Player.new(args)
                if let ExprKind::Name(tn) = &base.kind {
                    if self.lookup(tn).is_none() && (self.struct_ids.contains_key(tn) || self.enum_ids.contains_key(tn) || self.generic_structs.contains_key(tn) || self.generic_enums.contains_key(tn)) {
                        if let Some(ge) = self.generic_enums.get(tn).cloned() {
                            if let Some(vi) = ge.variants.iter().position(|v| v.name == *name) {
                                return self.construct_generic_variant(tn, vi, args, span, expected);
                            }
                        }
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
                if matches!(name.as_str(), "has" | "get" | "remove" | "keys" | "values" | "clear" | "add" | "items") {
                    return self.collection_method(base, name, args, span);
                }
                Err(())
            }
            _ => Err(()),
        }
    }

    /// The type of a simple expression (variable, field, element) without checking it, or None.
    pub(crate) fn static_type(&mut self, e: &Expr) -> Option<Type> {
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
                Type::Map(_, v) => Some(*v),
                _ => None,
            },
            _ => None,
        }
    }

    /// The function index of method `name` of struct `owner`, defined in this module or any other.
    fn find_method(&mut self, owner: &str, name: &str) -> Option<usize> {
        let owner = base_name(owner);
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

    /// The implicit `self: Pair` of a method on a generic type: it stands for any `Pair[...]`.
    fn is_generic_self(&self, p: &Param) -> bool {
        p.name == "self" && p.ty.as_ref().is_some_and(|t| t.args.is_empty() && (self.generic_structs.contains_key(&t.name) || self.generic_enums.contains_key(&t.name)))
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
                Some(_) if self.is_generic_self(p) => None,
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

    fn new_closure_target(&mut self, func: FuncId, ty: Type, captures: usize) -> usize {
        self.closures.push(ClosureTarget { func, ty, captures });
        self.closures.len() - 1
    }

    /// `lambda x, y: body` where the parameter types come from the expected `Fn` type.
    fn lambda(&mut self, params: &[LambdaParam], body: &Expr, expected: Option<&Type>, span: Span) -> R<HExpr> {
        let (ptys, ret): (Vec<Type>, Type) = match expected {
            Some(Type::Func(ps, r)) if ps.len() == params.len() => (ps.clone(), (**r).clone()),
            Some(Type::Func(ps, _)) => {
                return self.fail(Diagnostic::error(span, format!("This lambda takes {} parameter(s), but {} are expected here.", params.len(), ps.len())));
            }
            _ => {
                return self.fail(
                    Diagnostic::error(span, "I can't tell what type of function this lambda should be.")
                        .help("Give it a type: `f: Fn(Int) -> Int = lambda x: x + 1`, or pass it where a function is expected."),
                );
            }
        };
        // explicit parameter types must agree with the expected ones
        for (p, t) in params.iter().zip(ptys.iter()) {
            if let Some(te) = &p.ty {
                let declared = self.resolve_type(te)?;
                if declared != *t {
                    let (a, b) = (self.tn(&declared), self.tn(t));
                    return self.fail(Diagnostic::error(p.span, format!("The parameter '{}' is {}, but {} is expected here.", p.name, a, b)));
                }
            }
        }
        let moving = std::mem::take(&mut self.move_lambda);
        let fty = Type::Func(ptys.clone(), Box::new(ret.clone()));
        let (module, inst_name) = {
            let cx = self.cx();
            (cx.module, format!("lambda@{}", span.lo))
        };
        let id = self.new_func_slot();
        self.ret_of[id] = Some(ret.clone());
        self.stack.push(FnCtx {
            locals: vec![],
            scopes: vec![HashMap::new()],
            ret: Some(ret.clone()),
            ret_declared: true,
            loop_depth: 0,
            is_main: false,
            module,
            inst: id,
            call_site: None,
            lambda: true,
            captures: vec![],
            moving,
        });
        let env = self.add_local("<closure>", fty.clone(), LocalKind::ParamBorrow, span);
        let mut plist = vec![env];
        for (p, t) in params.iter().zip(ptys.iter()) {
            let kind = if t.is_copy() { LocalKind::Var } else { LocalKind::ParamBorrow };
            plist.push(self.add_local(&p.name, t.clone(), kind, p.span));
        }
        let hb = self.expr(body, if ret == Type::None { None } else { Some(&ret) }, Ctx::Value);
        let mut stmts = vec![];
        let mut ok = hb.is_ok();
        if let Ok(h) = hb {
            if ret == Type::None {
                stmts.push(HStmt { kind: HStmtKind::Expr(h), span: body.span });
            } else if h.ty != ret && h.ty != Type::Error {
                let _ = self.type_mismatch::<()>(body.span, &ret, &h, "The result of the lambda");
                ok = false;
            } else {
                stmts.push(HStmt { kind: HStmtKind::Return(Some(h)), span: body.span });
            }
        }
        let cx = self.stack.pop().unwrap();
        if !ok {
            return Err(());
        }
        // captured values are copied out of the closure block when the lambda starts
        let mut full = vec![];
        for (i, (_, inner)) in cx.captures.iter().enumerate() {
            let ty = cx.locals[*inner].ty.clone();
            let get = he(HKind::Capture(Box::new(he(HKind::Local(env, UseMode::Borrow), fty.clone(), span)), i), ty, span);
            full.push(HStmt { kind: HStmtKind::Let(*inner, get), span });
        }
        full.extend(stmts);
        self.funcs[id] = Some(HFunc { name: inst_name, params: plist, locals: cx.locals, ret, body: full, span, is_main: false });
        let target = self.new_closure_target(id, fty.clone(), cx.captures.len());
        let mut caps = vec![];
        for (outer, _) in &cx.captures {
            let l = &self.cx().locals[*outer];
            let (ty, name, kind) = (l.ty.clone(), l.name.clone(), l.kind);
            if ty.is_copy() {
                caps.push(he(HKind::Local(*outer, UseMode::Borrow), ty, span));
            } else if !kind.owns() {
                return self.fail(
                    Diagnostic::error(span, format!("'{}' is only borrowed here, so it can't be moved into the thread.", name))
                        .help(format!("Make a copy for the thread: `copy({})`, or take the parameter with `own`.", name)),
                );
            } else {
                caps.push(he(HKind::Local(*outer, UseMode::Move), ty, span));
            }
        }
        Ok(he(HKind::Closure { target, captures: caps }, fty, span))
    }

    /// A named function used as a value: `apply(double, 3)`.
    fn fn_value(&mut self, fidx: usize, expected: Option<&Type>, span: Span) -> R<HExpr> {
        let def = self.fns[fidx].def.clone();
        let Some(Type::Func(ptys, ret)) = expected.cloned() else {
            return self.fail(
                Diagnostic::error(span, format!("'{}' is a function, not a value.", def.name))
                    .help(format!("Call it with parentheses: {}(...). To pass it as a value, the receiver needs a `Fn(...)` parameter type.", def.name)),
            );
        };
        if def.params.len() != ptys.len() {
            return self.fail(Diagnostic::error(
                span,
                format!("'{}' takes {} parameter(s), but a function of {} is expected here.", def.name, def.params.len(), ptys.len()),
            ));
        }
        if let Some(p) = def.params.iter().find(|p| p.mode != ParamMode::Borrow) {
            return self.fail(Diagnostic::error(span, format!("'{}' can't be used as a value: its parameter '{}' is `own` or `mut`.", def.name, p.name)));
        }
        let fid = self.instantiate(fidx, ptys.clone(), span)?;
        let got = self.ret_of[fid].clone().unwrap_or(Type::None);
        if got != *ret {
            let (a, b) = (self.tn(&got), self.tn(&ret));
            return self.fail(Diagnostic::error(span, format!("'{}' returns {}, but {} is expected here.", def.name, a, b)));
        }
        let fty = Type::Func(ptys.clone(), ret.clone());
        let target = match self.fn_values.get(&fid) {
            Some(&t) => t,
            None => {
                // a wrapper that takes the (unused) closure block first
                let w = self.new_func_slot();
                self.ret_of[w] = Some(*ret.clone());
                let mut locals = vec![LocalInfo { name: "<closure>".into(), ty: fty.clone(), kind: LocalKind::ParamBorrow, span }];
                let mut params = vec![0];
                let mut args = vec![];
                for (i, t) in ptys.iter().enumerate() {
                    let kind = if t.is_copy() { LocalKind::Var } else { LocalKind::ParamBorrow };
                    locals.push(LocalInfo { name: format!("a{}", i), ty: t.clone(), kind, span });
                    params.push(locals.len() - 1);
                    args.push(he(HKind::Local(locals.len() - 1, UseMode::Borrow), t.clone(), span));
                }
                let call = he(HKind::Call(fid, args), *ret.clone(), span);
                let stmt = if *ret == Type::None { HStmtKind::Expr(call) } else { HStmtKind::Return(Some(call)) };
                self.funcs[w] = Some(HFunc {
                    name: format!("{}@value", def.name),
                    params,
                    locals,
                    ret: *ret.clone(),
                    body: vec![HStmt { kind: stmt, span }],
                    span,
                    is_main: false,
                });
                let t = self.new_closure_target(w, fty.clone(), 0);
                self.fn_values.insert(fid, t);
                t
            }
        };
        Ok(he(HKind::Closure { target, captures: vec![] }, fty, span))
    }

    /// `f(args)` where `f` is a variable or parameter holding a function value.
    fn call_value(&mut self, id: LocalId, name: &str, args: &[Arg], span: Span) -> R<HExpr> {
        let ty = self.cx().locals[id].ty.clone();
        let f = he(HKind::Local(id, UseMode::Borrow), ty, span);
        self.call_fn_expr(f, name, args, span)
    }

    fn call_fn_expr(&mut self, f: HExpr, name: &str, args: &[Arg], span: Span) -> R<HExpr> {
        let ty = f.ty.clone();
        let Type::Func(ptys, ret) = ty.clone() else {
            if ty == Type::Error {
                return Err(());
            }
            let tn = self.tn(&ty);
            return self.fail(Diagnostic::error(span, format!("'{}' is a variable of type {}, not a function, so it cannot be called.", name, tn)));
        };
        if let Some(a) = args.iter().find(|a| a.name.is_some()) {
            return self.fail(Diagnostic::error(a.value.span, "Function values take their arguments by position, not by name."));
        }
        if args.len() != ptys.len() {
            return self.fail(Diagnostic::error(
                span,
                format!("'{}' takes {} argument(s), but {} were given.", name, ptys.len(), args.len()),
            ));
        }
        let mut hs = vec![];
        let mut failed = false;
        for (a, t) in args.iter().zip(ptys.iter()) {
            match self.expr(&a.value, Some(t), Ctx::Borrow) {
                Ok(h) => {
                    if h.ty != *t && h.ty != Type::Error {
                        let _ = self.type_mismatch::<()>(a.value.span, t, &h, "This argument");
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
        Ok(he(HKind::CallValue(Box::new(f), hs), *ret, span))
    }

    /// `Pair[Int, String]`: the concrete struct or enum for a generic template.
    fn instantiate_generic(&mut self, name: &str, args: Vec<Type>, span: Span) -> R<Type> {
        let params = match (self.generic_structs.get(name), self.generic_enums.get(name)) {
            (Some(s), _) => s.params.clone(),
            (_, Some(e)) => e.params.clone(),
            _ => return Err(()),
        };
        if params.len() != args.len() {
            return self.fail(
                Diagnostic::error(span, format!("{} needs {} type argument(s), but {} given.", name, params.len(), args.len()))
                    .help(format!("Write it as {}[{}].", name, params.join(", "))),
            );
        }
        let inner: Vec<String> = args.iter().map(|t| self.tn(t)).collect();
        let full = format!("{}[{}]", name, inner.join(", "));
        if let Some(&id) = self.struct_ids.get(&full) {
            return Ok(Type::Struct(id));
        }
        if let Some(&id) = self.enum_ids.get(&full) {
            return Ok(Type::Enum(id));
        }
        let saved = std::mem::replace(&mut self.tsubst, params.iter().cloned().zip(args).collect());
        let result = if let Some(s) = self.generic_structs.get(name).cloned() {
            let id = self.structs.len();
            self.struct_ids.insert(full.clone(), id);
            self.structs.push(StructInfo { name: full, fields: vec![], span: s.name_span, offsets: vec![], size: 8, tuple: false });
            let mut fields = vec![];
            for f in &s.fields {
                if let Ok(t) = self.resolve_type(&f.ty) {
                    fields.push((f.name.clone(), t));
                }
            }
            self.structs[id].fields = fields;
            self.structs[id].compute_layout();
            Type::Struct(id)
        } else {
            let e = self.generic_enums.get(name).cloned().unwrap();
            let id = self.enums.len();
            self.enum_ids.insert(full.clone(), id);
            self.enums.push(EnumInfo { name: full, variants: vec![], span: e.name_span, size: 8 });
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
            self.enums[id].compute_layout();
            Type::Enum(id)
        };
        self.tsubst = saved;
        Ok(result)
    }

    /// The generic enum that has an unqualified variant `name` (if exactly one does).
    fn generic_variant(&mut self, name: &str) -> Option<(String, usize)> {
        let module = self.cx().module;
        if self.fn_index.contains_key(&(module, name.to_string())) || self.struct_ids.contains_key(name) || self.unqualified_variant(name).is_some() {
            return None;
        }
        let mut found = None;
        for (en, e) in self.generic_enums.iter() {
            if let Some(vi) = e.variants.iter().position(|v| v.name == name) {
                if found.is_some() {
                    return None;
                }
                found = Some((en.clone(), vi));
            }
        }
        found
    }

    /// Work out the type parameters from the types of the given field values.
    fn unify(&self, te: &TypeExpr, ty: &Type, params: &[String], out: &mut HashMap<String, Type>) {
        if te.args.is_empty() && params.iter().any(|p| *p == te.name) {
            out.entry(te.name.clone()).or_insert_with(|| ty.clone());
            return;
        }
        match (te.name.as_str(), ty) {
            ("List", Type::List(i)) | ("Result", Type::Result(i)) | ("Set", Type::Set(i)) if te.args.len() == 1 => self.unify(&te.args[0], i, params, out),
            ("Map", Type::Map(k, v)) if te.args.len() == 2 => {
                self.unify(&te.args[0], k, params, out);
                self.unify(&te.args[1], v, params, out);
            }
            ("Tuple", Type::Struct(sid)) if self.structs[*sid].tuple && self.structs[*sid].fields.len() == te.args.len() => {
                for (a, (_, t)) in te.args.iter().zip(self.structs[*sid].fields.iter()) {
                    self.unify(a, t, params, out);
                }
            }
            _ => {}
        }
    }

    /// Can the type parameters be read off a value of this field type? (`T`, `List[T]`, ...; not `Tree[T]`)
    fn mentions(te: &TypeExpr, params: &[String]) -> bool {
        if params.iter().any(|p| *p == te.name) {
            return true;
        }
        matches!(te.name.as_str(), "List" | "Result" | "Set" | "Map" | "Tuple") && te.args.iter().any(|a| Self::mentions(a, params))
    }

    /// Check the arguments of a generic constructor, infer the type arguments from them, and
    /// return the concrete type plus the checked field values.
    fn infer_generic(&mut self, what: &str, name: &str, params: &[String], fields: &[FieldDef], vi: usize, args: &[Arg], span: Span, expected: Option<&Type>, noun: &str) -> R<(Type, Vec<HExpr>)> {
        let names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
        let list: Vec<(Option<&str>, &Expr)> = args.iter().map(|a| (a.name.as_deref(), &a.value)).collect();
        let bound = self.bind_args(what, &names, &list, span)?;
        let mut targs: HashMap<String, Type> = HashMap::new();
        // the expected type fixes every parameter at once
        let known = match expected {
            Some(t @ (Type::Struct(_) | Type::Enum(_))) => {
                let n = match t {
                    Type::Struct(s) => self.structs[*s].name.clone(),
                    Type::Enum(e) => self.enums[*e].name.clone(),
                    _ => unreachable!(),
                };
                (base_name(&n) == name && n.contains('[')).then(|| t.clone())
            }
            _ => None,
        };
        let mut pre: Vec<Option<HExpr>> = vec![None; fields.len()];
        let mut failed = false;
        if known.is_none() {
            for (i, (f, e)) in fields.iter().zip(bound.iter()).enumerate() {
                if Self::mentions(&f.ty, params) {
                    match self.expr(e, None, Ctx::Value) {
                        Ok(h) => {
                            self.unify(&f.ty, &h.ty, params, &mut targs);
                            pre[i] = Some(h);
                        }
                        Err(_) => failed = true,
                    }
                }
            }
            if failed {
                return Err(());
            }
            let mut tys = vec![];
            for p in params {
                match targs.get(p) {
                    Some(t) => tys.push(t.clone()),
                    None => {
                        return self.fail(
                            Diagnostic::error(span, format!("I can't tell what '{}' is for this {}.", p, name))
                                .help(format!("Say which type you want: `x: {}[...] = ...`.", name)),
                        );
                    }
                }
            }
            let ty = self.instantiate_generic(name, tys, span)?;
            return self.finish_generic(ty, vi, &bound, pre, noun);
        }
        self.finish_generic(known.unwrap(), vi, &bound, pre, noun)
    }

    /// With the concrete type known, check the remaining values against its field types.
    fn finish_generic(&mut self, ty: Type, vi: usize, bound: &[&Expr], pre: Vec<Option<HExpr>>, what: &str) -> R<(Type, Vec<HExpr>)> {
        let fields = match &ty {
            Type::Struct(s) => self.structs[*s].fields.clone(),
            Type::Enum(e) => self.enums[*e].variants[vi].fields.clone(),
            _ => return Err(()),
        };
        let mut hs = vec![];
        let mut failed = false;
        for (((fname, fty), e), p) in fields.iter().zip(bound.iter()).zip(pre) {
            let h = match p {
                Some(h) => Ok(h),
                None => self.expr(e, Some(fty), Ctx::Value),
            };
            match h {
                Ok(h) => {
                    if h.ty != *fty && h.ty != Type::Error {
                        let w = format!("The {} '{}' of {}", what, fname, self.tn(&ty));
                        let _ = self.type_mismatch::<()>(e.span, fty, &h, &w);
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
        Ok((ty, hs))
    }

    fn construct_generic_struct(&mut self, name: &str, args: &[Arg], span: Span, expected: Option<&Type>) -> R<HExpr> {
        let def = self.generic_structs.get(name).cloned().unwrap();
        let what = format!("The struct {}", name);
        let (ty, hs) = self.infer_generic(&what, name, &def.params, &def.fields, 0, args, span, expected, "field")?;
        Ok(he(HKind::Struct(hs), ty, span))
    }

    fn construct_generic_variant(&mut self, en: &str, vi: usize, args: &[Arg], span: Span, expected: Option<&Type>) -> R<HExpr> {
        let def = self.generic_enums.get(en).cloned().unwrap();
        let v = &def.variants[vi];
        let what = format!("The variant {}.{}", en, v.name);
        let (ty, hs) = self.infer_generic(&what, en, &def.params, &v.fields, vi, args, span, expected, "value")?;
        let Type::Enum(eid) = ty else { return Err(()) };
        Ok(he(HKind::Variant { enum_id: eid, variant: vi, fields: hs }, ty, span))
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

/// `Pair` for the instance `Pair[Int, String]` (tuple types keep their whole name).
fn base_name(n: &str) -> &str {
    if n.starts_with('(') {
        n
    } else {
        n.split('[').next().unwrap_or(n)
    }
}
