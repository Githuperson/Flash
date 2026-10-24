//! Type rules of the built-in functions.

use crate::ast::Expr;
use crate::check::*;
use crate::diag::Diagnostic;
use crate::hir::*;
use crate::source::Span;
use crate::types::*;

impl<'a> Checker<'a> {
    fn nargs(&mut self, name: &str, args: &[&Expr], n: usize, span: Span) -> R<()> {
        if args.len() != n {
            let msg = format!(
                "{}() takes {} argument{}, but {} {} given.",
                name,
                n,
                if n == 1 { "" } else { "s" },
                args.len(),
                if args.len() == 1 { "was" } else { "were" }
            );
            return self.fail(Diagnostic::error(span, msg));
        }
        Ok(())
    }

    fn arg_of(&mut self, e: &Expr, want: &Type, name: &str, what: &str, ctx: Ctx) -> R<HExpr> {
        let h = self.expr(e, Some(want), ctx)?;
        if h.ty != *want && h.ty != Type::Error {
            let what = format!("{} of {}()", what, name);
            return self.type_mismatch(e.span, want, &h, &what);
        }
        Ok(h)
    }

    fn mk(&self, b: Builtin, args: Vec<HExpr>, ty: Type, span: Span) -> HExpr {
        he(HKind::Builtin(b, args), ty, span)
    }

    fn printable(&mut self, name: &str, h: &HExpr, span: Span) -> R<()> {
        match h.ty {
            Type::Int(_) | Type::Float | Type::Bool | Type::Str | Type::None => Ok(()),
            _ => {
                let tn = self.tn(&h.ty);
                self.fail(
                    Diagnostic::error(span, format!("{}() cannot print a value of type {}.", name, tn))
                        .help("Print numbers, Bool, String and None directly; for lists use `for x in items: say(x)` or `join(items, \", \")`."),
                )
            }
        }
    }

    pub fn builtin(&mut self, name: &str, args: &[&Expr], span: Span, expected: Option<&Type>, _ctx: Ctx) -> R<HExpr> {
        use Builtin as B;
        let s_ty = Type::Str;
        match name {
            "say" | "print" => {
                let mut hs = vec![];
                for a in args {
                    let h = self.expr(a, None, Ctx::Borrow)?;
                    self.printable(name, &h, a.span)?;
                    hs.push(h);
                }
                Ok(self.mk(if name == "say" { B::Say } else { B::Print }, hs, Type::None, span))
            }
            "str" => {
                self.nargs(name, args, 1, span)?;
                let h = self.expr(args[0], None, Ctx::Borrow)?;
                self.printable(name, &h, args[0].span)?;
                Ok(self.mk(B::StrOf, vec![h], s_ty, span))
            }
            "int" | "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" => {
                self.nargs(name, args, 1, span)?;
                let target = IntTy::from_name(name).unwrap_or(INT);
                // A literal is checked against the target type, so `u64(18446744073709551615)` works.
                let lit_exp = Type::Int(target);
                let h = self.expr(args[0], if matches!(args[0].kind, crate::ast::ExprKind::Int(v) if v > i64::MAX as i128) { Some(&lit_exp) } else { None }, Ctx::Borrow)?;
                if !(h.ty.is_numeric() || h.ty == Type::Bool) {
                    let tn = self.tn(&h.ty);
                    let mut d = Diagnostic::error(args[0].span, format!("Cannot convert {} to {}.", tn, target.name()));
                    if h.ty == Type::Str {
                        d = d.help("Use `parse_int(text)` to turn text into a number; it returns a Result.");
                    }
                    return self.fail(d);
                }
                Ok(he(HKind::Cast(Box::new(h)), Type::Int(target), span))
            }
            "float" => {
                self.nargs(name, args, 1, span)?;
                let h = self.expr(args[0], None, Ctx::Borrow)?;
                if !h.ty.is_numeric() {
                    let tn = self.tn(&h.ty);
                    let mut d = Diagnostic::error(args[0].span, format!("Cannot convert {} to Float.", tn));
                    if h.ty == Type::Str {
                        d = d.help("Use `parse_float(text)` to turn text into a number; it returns a Result.");
                    }
                    return self.fail(d);
                }
                Ok(he(HKind::Cast(Box::new(h)), Type::Float, span))
            }
            "len" => {
                self.nargs(name, args, 1, span)?;
                let h = self.expr(args[0], None, Ctx::Borrow)?;
                if !matches!(h.ty, Type::Str | Type::List(_)) {
                    let tn = self.tn(&h.ty);
                    return self.fail(Diagnostic::error(args[0].span, format!("len() works on String and List, but this is {}.", tn)));
                }
                Ok(self.mk(B::Len, vec![h], Type::Int(INT), span))
            }
            "append" => {
                self.nargs(name, args, 2, span)?;
                let l = self.expr(args[0], None, Ctx::Borrow)?;
                let Type::List(et) = l.ty.clone() else {
                    let tn = self.tn(&l.ty);
                    return self.fail(Diagnostic::error(args[0].span, format!("append() needs a List, but this is {}.", tn)));
                };
                self.check_mut_place(&l, args[0].span, "append to")?;
                let v = self.arg_of(args[1], &et, name, "The value", Ctx::Value)?;
                Ok(self.mk(B::Append, vec![l, v], Type::None, span))
            }
            "pop" => {
                self.nargs(name, args, 1, span)?;
                let l = self.expr(args[0], None, Ctx::Borrow)?;
                let Type::List(et) = l.ty.clone() else {
                    let tn = self.tn(&l.ty);
                    return self.fail(Diagnostic::error(args[0].span, format!("pop() needs a List, but this is {}.", tn)));
                };
                self.check_mut_place(&l, args[0].span, "pop from")?;
                Ok(self.mk(B::Pop, vec![l], *et, span))
            }
            "copy" => {
                self.nargs(name, args, 1, span)?;
                let h = self.expr(args[0], None, Ctx::Borrow)?;
                let ty = h.ty.clone();
                Ok(self.mk(B::Copy, vec![h], ty, span))
            }
            "abs" => {
                self.nargs(name, args, 1, span)?;
                let h = self.expr(args[0], expected, Ctx::Borrow)?;
                if !(h.ty == Type::Float || matches!(h.ty, Type::Int(i) if i.signed)) {
                    let tn = self.tn(&h.ty);
                    return self.fail(Diagnostic::error(args[0].span, format!("abs() needs a signed integer or Float, but this is {}.", tn)));
                }
                let ty = h.ty.clone();
                Ok(self.mk(B::Abs, vec![h], ty, span))
            }
            "min" | "max" => {
                self.nargs(name, args, 2, span)?;
                let (a, b) = if matches!(args[0].kind, crate::ast::ExprKind::Int(_)) && !matches!(args[1].kind, crate::ast::ExprKind::Int(_)) {
                    let b = self.expr(args[1], expected, Ctx::Borrow)?;
                    let a = self.expr(args[0], Some(&b.ty), Ctx::Borrow)?;
                    (a, b)
                } else {
                    let a = self.expr(args[0], expected, Ctx::Borrow)?;
                    let b = self.expr(args[1], Some(&a.ty), Ctx::Borrow)?;
                    (a, b)
                };
                if !a.ty.is_numeric() || a.ty != b.ty {
                    let (x, y) = (self.tn(&a.ty), self.tn(&b.ty));
                    return self.fail(Diagnostic::error(span, format!("{}() needs two numbers of the same type, but these are {} and {}.", name, x, y)));
                }
                let ty = a.ty.clone();
                Ok(self.mk(if name == "min" { B::Min } else { B::Max }, vec![a, b], ty, span))
            }
            "sqrt" | "floor" | "ceil" | "sin" | "cos" | "tan" | "exp" | "ln" => {
                self.nargs(name, args, 1, span)?;
                let h = self.arg_of(args[0], &Type::Float, name, "The argument", Ctx::Borrow)?;
                let f: &'static str = match name {
                    "sqrt" => "sqrt",
                    "floor" => "floor",
                    "ceil" => "ceil",
                    "sin" => "sin",
                    "cos" => "cos",
                    "tan" => "tan",
                    "exp" => "exp",
                    _ => "ln",
                };
                Ok(self.mk(B::MathF(f), vec![h], Type::Float, span))
            }
            "pow" => {
                self.nargs(name, args, 2, span)?;
                let a = self.arg_of(args[0], &Type::Float, name, "The base", Ctx::Borrow)?;
                let b = self.arg_of(args[1], &Type::Float, name, "The exponent", Ctx::Borrow)?;
                Ok(self.mk(B::Pow, vec![a, b], Type::Float, span))
            }
            "exit" => {
                self.nargs(name, args, 1, span)?;
                let a = self.arg_of(args[0], &Type::Int(INT), name, "The exit code", Ctx::Borrow)?;
                Ok(self.mk(B::Exit, vec![a], Type::None, span))
            }
            "assert" => {
                if args.is_empty() || args.len() > 2 {
                    return self.fail(Diagnostic::error(span, "assert() takes a condition and an optional message."));
                }
                let mut hs = vec![self.arg_of(args[0], &Type::Bool, name, "The condition", Ctx::Borrow)?];
                if args.len() == 2 {
                    hs.push(self.arg_of(args[1], &Type::Str, name, "The message", Ctx::Borrow)?);
                }
                Ok(self.mk(B::Assert, hs, Type::None, span))
            }
            "now_ns" => {
                self.nargs(name, args, 0, span)?;
                Ok(self.mk(B::NowNs, vec![], Type::Int(INT), span))
            }
            "args" => {
                self.nargs(name, args, 0, span)?;
                Ok(self.mk(B::Args, vec![], Type::List(Box::new(Type::Str)), span))
            }
            "read_line" => {
                self.nargs(name, args, 0, span)?;
                Ok(self.mk(B::ReadLine, vec![], Type::Str, span))
            }
            "sleep_ms" => {
                self.nargs(name, args, 1, span)?;
                let a = self.arg_of(args[0], &Type::Int(INT), name, "The duration", Ctx::Borrow)?;
                Ok(self.mk(B::SleepMs, vec![a], Type::None, span))
            }
            "upper" | "lower" | "trim" | "path_filename" | "path_parent" | "path_extension" => {
                self.nargs(name, args, 1, span)?;
                let a = self.arg_of(args[0], &s_ty, name, "The argument", Ctx::Borrow)?;
                let b = match name {
                    "upper" => B::Upper,
                    "lower" => B::Lower,
                    "trim" => B::Trim,
                    "path_filename" => B::PathFilename,
                    "path_parent" => B::PathParent,
                    _ => B::PathExt,
                };
                Ok(self.mk(b, vec![a], s_ty, span))
            }
            "contains" | "starts_with" | "ends_with" => {
                self.nargs(name, args, 2, span)?;
                let a = self.arg_of(args[0], &s_ty, name, "The first argument", Ctx::Borrow)?;
                let b = self.arg_of(args[1], &s_ty, name, "The second argument", Ctx::Borrow)?;
                let bi = match name {
                    "contains" => B::Contains,
                    "starts_with" => B::StartsWith,
                    _ => B::EndsWith,
                };
                Ok(self.mk(bi, vec![a, b], Type::Bool, span))
            }
            "find" => {
                self.nargs(name, args, 2, span)?;
                let a = self.arg_of(args[0], &s_ty, name, "The first argument", Ctx::Borrow)?;
                let b = self.arg_of(args[1], &s_ty, name, "The second argument", Ctx::Borrow)?;
                Ok(self.mk(B::Find, vec![a, b], Type::Int(INT), span))
            }
            "replace" => {
                self.nargs(name, args, 3, span)?;
                let mut hs = vec![];
                for a in args {
                    hs.push(self.arg_of(a, &s_ty, name, "Every argument", Ctx::Borrow)?);
                }
                Ok(self.mk(B::Replace, hs, s_ty, span))
            }
            "split" => {
                self.nargs(name, args, 2, span)?;
                let a = self.arg_of(args[0], &s_ty, name, "The text", Ctx::Borrow)?;
                let b = self.arg_of(args[1], &s_ty, name, "The separator", Ctx::Borrow)?;
                Ok(self.mk(B::Split, vec![a, b], Type::List(Box::new(Type::Str)), span))
            }
            "join" => {
                self.nargs(name, args, 2, span)?;
                let a = self.arg_of(args[0], &Type::List(Box::new(Type::Str)), name, "The list", Ctx::Borrow)?;
                let b = self.arg_of(args[1], &s_ty, name, "The separator", Ctx::Borrow)?;
                Ok(self.mk(B::Join, vec![a, b], s_ty, span))
            }
            "substr" => {
                self.nargs(name, args, 3, span)?;
                let a = self.arg_of(args[0], &s_ty, name, "The text", Ctx::Borrow)?;
                let b = self.arg_of(args[1], &Type::Int(INT), name, "The start", Ctx::Borrow)?;
                let c = self.arg_of(args[2], &Type::Int(INT), name, "The length", Ctx::Borrow)?;
                Ok(self.mk(B::Substr, vec![a, b, c], s_ty, span))
            }
            "byte_at" => {
                self.nargs(name, args, 2, span)?;
                let a = self.arg_of(args[0], &s_ty, name, "The text", Ctx::Borrow)?;
                let b = self.arg_of(args[1], &Type::Int(INT), name, "The index", Ctx::Borrow)?;
                Ok(self.mk(B::ByteAt, vec![a, b], Type::Int(IntTy { bits: 8, signed: false }), span))
            }
            "parse_int" | "parse_float" => {
                self.nargs(name, args, 1, span)?;
                let a = self.arg_of(args[0], &s_ty, name, "The text", Ctx::Borrow)?;
                if name == "parse_int" {
                    Ok(self.mk(B::ParseInt, vec![a], Type::Result(Box::new(Type::Int(INT))), span))
                } else {
                    Ok(self.mk(B::ParseFloat, vec![a], Type::Result(Box::new(Type::Float)), span))
                }
            }
            "read_file" | "env_var" => {
                self.nargs(name, args, 1, span)?;
                let a = self.arg_of(args[0], &s_ty, name, "The argument", Ctx::Borrow)?;
                let b = if name == "read_file" { B::ReadFile } else { B::EnvVar };
                Ok(self.mk(b, vec![a], Type::Result(Box::new(Type::Str)), span))
            }
            "write_file" | "append_file" => {
                self.nargs(name, args, 2, span)?;
                let a = self.arg_of(args[0], &s_ty, name, "The path", Ctx::Borrow)?;
                let b = self.arg_of(args[1], &s_ty, name, "The text", Ctx::Borrow)?;
                let bi = if name == "write_file" { B::WriteFile } else { B::AppendFile };
                Ok(self.mk(bi, vec![a, b], Type::Result(Box::new(Type::None)), span))
            }
            "remove_file" => {
                self.nargs(name, args, 1, span)?;
                let a = self.arg_of(args[0], &s_ty, name, "The path", Ctx::Borrow)?;
                Ok(self.mk(B::RemoveFile, vec![a], Type::Result(Box::new(Type::None)), span))
            }
            "file_exists" => {
                self.nargs(name, args, 1, span)?;
                let a = self.arg_of(args[0], &s_ty, name, "The path", Ctx::Borrow)?;
                Ok(self.mk(B::FileExists, vec![a], Type::Bool, span))
            }
            "path_join" => {
                self.nargs(name, args, 2, span)?;
                let a = self.arg_of(args[0], &s_ty, name, "The first path", Ctx::Borrow)?;
                let b = self.arg_of(args[1], &s_ty, name, "The second path", Ctx::Borrow)?;
                Ok(self.mk(B::PathJoin, vec![a, b], s_ty, span))
            }
            "ok" => {
                if args.len() > 1 {
                    return self.fail(Diagnostic::error(span, "ok() takes zero or one argument."));
                }
                if args.is_empty() {
                    return Ok(self.mk(B::Ok, vec![], Type::Result(Box::new(Type::None)), span));
                }
                let inner_exp = match expected {
                    Some(Type::Result(t)) => Some((**t).clone()),
                    _ => None,
                };
                let h = self.expr(args[0], inner_exp.as_ref(), Ctx::Value)?;
                let ty = Type::Result(Box::new(h.ty.clone()));
                Ok(self.mk(B::Ok, vec![h], ty, span))
            }
            "fail" => {
                self.nargs(name, args, 1, span)?;
                let m = self.arg_of(args[0], &s_ty, name, "The message", Ctx::Borrow)?;
                let Some(Type::Result(t)) = expected else {
                    return self.fail(
                        Diagnostic::error(span, "I can't tell which kind of Result this fail(...) belongs to.")
                            .help("Declare the function's return type, for example `fn load(path) -> Result[String]:`, then `return fail(\"message\")` works."),
                    );
                };
                let ty = Type::Result(t.clone());
                Ok(self.mk(B::Fail, vec![m], ty, span))
            }
            "is_ok" | "error_message" | "unwrap" | "unwrap_or" => {
                let want = if name == "unwrap_or" { 2 } else { 1 };
                self.nargs(name, args, want, span)?;
                let consume = matches!(name, "unwrap" | "unwrap_or");
                let r = self.expr(args[0], None, if consume { Ctx::Value } else { Ctx::Borrow })?;
                let Type::Result(t) = r.ty.clone() else {
                    let tn = self.tn(&r.ty);
                    return self.fail(Diagnostic::error(args[0].span, format!("{}() needs a Result, but this is {}.", name, tn)));
                };
                match name {
                    "is_ok" => Ok(self.mk(B::IsOk, vec![r], Type::Bool, span)),
                    "error_message" => Ok(self.mk(B::ErrorMsg, vec![r], s_ty, span)),
                    "unwrap" => Ok(self.mk(B::Unwrap, vec![r], *t, span)),
                    _ => {
                        let d = self.arg_of(args[1], &t, name, "The default", Ctx::Value)?;
                        Ok(self.mk(B::UnwrapOr, vec![r, d], *t, span))
                    }
                }
            }
            _ => self.fail(Diagnostic::error(span, format!("The built-in '{}' is not implemented.", name))),
        }
    }
}
