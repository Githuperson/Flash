//! Lowering of built-in function calls.

use super::*;

impl<'a, 'p> FnLower<'a, 'p> {
    fn print_value(&mut self, a: &HExpr) {
        let v = self.borrow(a);
        match &a.ty {
            Type::Int(it) => {
                let w = self.widen(v, *it);
                self.rt(if it.signed { "fl_print_int" } else { "fl_print_uint" }, &[w]);
            }
            Type::Float => {
                self.rt("fl_print_float", &[v]);
            }
            Type::Bool => {
                self.rt("fl_print_bool", &[v]);
            }
            Type::Str => {
                self.rt("fl_print_str", &[v]);
            }
            Type::None => {
                self.rt("fl_print_none", &[]);
            }
            Type::List(et) => self.print_list(v, et),
            t => unreachable!("print {:?}", t),
        }
    }

    /// Print `[a, b, c]` (strings are quoted) by looping over the elements.
    fn print_list(&mut self, list: VReg, et: &Type) {
        let open = self.lw.string_data("[");
        let close = self.lw.string_data("]");
        let sep = self.lw.string_data(", ");
        let quote = self.lw.string_data("\"");
        let lit = |s: &mut Self, d: DataId| {
            let p = s.vreg(Ty::I64);
            s.emit(Inst::DataAddr { dst: p, data: d });
            s.rt("fl_print_str", &[p]);
        };
        lit(self, open);
        let n = self.load(Ty::I64, list, 0);
        let idx = self.iconst(Ty::I64, 0);
        let head = self.new_block();
        let body = self.new_block();
        let exit = self.new_block();
        self.jump_to(head);
        self.switch(head);
        let c = self.cmp(CmpOp::SLt, idx, n);
        self.branch(c, body, exit);
        self.switch(body);
        let zero = self.iconst(Ty::I64, 0);
        let first = self.cmp(CmpOp::Eq, idx, zero);
        let not_first = self.new_block();
        let after_sep = self.new_block();
        self.branch(first, after_sep, not_first);
        self.switch(not_first);
        lit(self, sep);
        self.jump_to(after_sep);
        self.switch(after_sep);
        let data = self.load(Ty::I64, list, 16);
        let stride = self.iconst(Ty::I64, Self::stride(et));
        let off = self.bin(BinOp::Mul, idx, stride);
        let addr = self.bin(BinOp::Add, data, off);
        let ev = self.load(ir_ty(et), addr, 0);
        match et {
            Type::Int(it) => {
                let w = self.widen(ev, *it);
                self.rt(if it.signed { "fl_print_int" } else { "fl_print_uint" }, &[w]);
            }
            Type::Float => {
                self.rt("fl_print_float", &[ev]);
            }
            Type::Bool => {
                self.rt("fl_print_bool", &[ev]);
            }
            Type::Str => {
                lit(self, quote);
                self.rt("fl_print_str", &[ev]);
                lit(self, quote);
            }
            Type::List(inner) => self.print_list(ev, inner),
            _ => {
                self.rt("fl_print_none", &[]);
            }
        }
        let one = self.iconst(Ty::I64, 1);
        let next = self.bin(BinOp::Add, idx, one);
        self.copy(idx, next);
        self.jump_to(head);
        self.switch(exit);
        lit(self, close);
    }

    pub(super) fn builtin(&mut self, b: &Builtin, args: &[HExpr], e: &HExpr) -> VReg {
        use Builtin as B;
        match b {
            B::Say | B::Print => {
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        self.rt("fl_print_space", &[]);
                    }
                    self.print_value(a);
                }
                if *b == B::Say {
                    self.rt("fl_print_nl", &[]);
                }
                self.dummy()
            }
            B::StrOf => {
                let a = &args[0];
                match &a.ty {
                    Type::Str => {
                        let v = self.expr(a);
                        if crate::check::is_place(a) {
                            self.rtv("fl_str_clone", &[v])
                        } else {
                            v
                        }
                    }
                    Type::Int(it) => {
                        let v = self.borrow(a);
                        let w = self.widen(v, *it);
                        self.rtv(if it.signed { "fl_str_from_int" } else { "fl_str_from_uint" }, &[w])
                    }
                    Type::Float => {
                        let v = self.borrow(a);
                        self.rtv("fl_str_from_float", &[v])
                    }
                    Type::Bool => {
                        let v = self.borrow(a);
                        self.rtv("fl_str_from_bool", &[v])
                    }
                    _ => {
                        self.borrow(a);
                        self.rtv("fl_str_from_none", &[])
                    }
                }
            }
            B::Len => {
                let v = self.borrow(&args[0]);
                self.load(Ty::I64, v, 0)
            }
            B::Append => {
                let Type::List(et) = &args[0].ty else { unreachable!() };
                let l = self.borrow(&args[0]);
                let v = self.expr(&args[1]);
                let stride = self.iconst(Ty::I64, Self::stride(et));
                let slot = self.rtv("fl_list_push_slot", &[l, stride]);
                self.store(slot, 0, v);
                self.dummy()
            }
            B::Pop => {
                let Type::List(et) = &args[0].ty else { unreachable!() };
                let l = self.borrow(&args[0]);
                let stride = self.iconst(Ty::I64, Self::stride(et));
                let slot = self.rtv("fl_list_pop_slot", &[l, stride]);
                self.load(ir_ty(&e.ty), slot, 0)
            }
            B::Copy => {
                let v = self.borrow(&args[0]);
                self.emit_clone(&args[0].ty, v)
            }
            B::Abs => {
                let v = self.borrow(&args[0]);
                match &args[0].ty {
                    Type::Float => self.rtv("fl_math_abs", &[v]),
                    t => {
                        let ty = ir_ty(t);
                        let z = self.iconst(ty, 0);
                        let neg = self.cmp(CmpOp::SLt, v, z);
                        let n = self.vreg(ty);
                        self.emit(Inst::Un { dst: n, op: UnOp::Neg, a: v });
                        let r = self.vreg(ty);
                        self.emit(Inst::Select { dst: r, cond: neg, a: n, b: v });
                        r
                    }
                }
            }
            B::Min | B::Max => {
                let a = self.borrow(&args[0]);
                let b2 = self.borrow(&args[1]);
                let ty = ir_ty(&args[0].ty);
                let is_min = *b == B::Min;
                let op = match &args[0].ty {
                    Type::Float => if is_min { CmpOp::FLt } else { CmpOp::FGt },
                    Type::Int(it) if it.signed => if is_min { CmpOp::SLt } else { CmpOp::SGt },
                    _ => if is_min { CmpOp::ULt } else { CmpOp::UGt },
                };
                let c = self.cmp(op, a, b2);
                let r = self.vreg(ty);
                self.emit(Inst::Select { dst: r, cond: c, a, b: b2 });
                r
            }
            B::MathF(name) => {
                let v = self.borrow(&args[0]);
                self.rtv(&format!("fl_math_{}", name), &[v])
            }
            B::Pow => {
                let a = self.borrow(&args[0]);
                let b2 = self.borrow(&args[1]);
                self.rtv("fl_math_pow", &[a, b2])
            }
            B::Exit => {
                let c = self.expr(&args[0]);
                self.rt("fl_exit", &[c]);
                self.dummy()
            }
            B::Assert => {
                let c = self.expr(&args[0]);
                // Evaluate the message before branching so its temporary is defined on every path.
                let msg = if args.len() > 1 { self.borrow(&args[1]) } else { self.iconst(Ty::I64, 0) };
                let fail = self.new_block();
                let ok = self.new_block();
                self.branch(c, ok, fail);
                self.switch(fail);
                self.rt("fl_assert_fail", &[msg]);
                self.term(Term::Unreachable);
                self.switch(ok);
                self.dummy()
            }
            B::NowNs => self.rtv("fl_now_ns", &[]),
            B::Args => self.rtv("fl_args", &[]),
            B::ReadLine => self.rtv("fl_read_line", &[]),
            B::SleepMs => {
                let v = self.expr(&args[0]);
                self.rt("fl_sleep_ms", &[v]);
                self.dummy()
            }
            B::Upper | B::Lower | B::Trim | B::PathFilename | B::PathParent | B::PathExt | B::ParseInt | B::ParseFloat
            | B::ReadFile | B::RemoveFile | B::EnvVar | B::FileExists => {
                let v = self.borrow(&args[0]);
                let name = match b {
                    B::Upper => "fl_str_upper",
                    B::Lower => "fl_str_lower",
                    B::Trim => "fl_str_trim",
                    B::PathFilename => "fl_path_filename",
                    B::PathParent => "fl_path_parent",
                    B::PathExt => "fl_path_ext",
                    B::ParseInt => "fl_parse_int",
                    B::ParseFloat => "fl_parse_float",
                    B::ReadFile => "fl_read_file",
                    B::RemoveFile => "fl_remove_file",
                    B::EnvVar => "fl_env_var",
                    _ => "fl_file_exists",
                };
                self.rtv(name, &[v])
            }
            B::Contains | B::StartsWith | B::EndsWith | B::Replace | B::Split | B::Join | B::Find | B::Substr | B::ByteAt
            | B::WriteFile | B::AppendFile | B::PathJoin => {
                let vs: Vec<VReg> = args.iter().map(|a| self.borrow(a)).collect();
                let name = match b {
                    B::Contains => "fl_str_contains",
                    B::StartsWith => "fl_str_starts_with",
                    B::EndsWith => "fl_str_ends_with",
                    B::Replace => "fl_str_replace",
                    B::Split => "fl_str_split",
                    B::Join => "fl_str_join",
                    B::Find => "fl_str_find",
                    B::Substr => "fl_str_substr",
                    B::ByteAt => "fl_str_byte_at",
                    B::WriteFile => "fl_write_file",
                    B::AppendFile => "fl_append_file",
                    _ => "fl_path_join",
                };
                self.rtv(name, &vs)
            }
            B::Ok => {
                let r = self.result_alloc(0);
                if let Some(a) = args.first() {
                    let v = self.expr(a);
                    if a.ty != Type::None {
                        self.store(r, 8, v);
                    }
                }
                r
            }
            B::Fail => {
                let m = self.borrow(&args[0]);
                let c = self.rtv("fl_str_clone", &[m]);
                let r = self.result_alloc(1);
                self.store(r, 16, c);
                r
            }
            B::IsOk => {
                let r = self.borrow(&args[0]);
                let tag = self.load(Ty::I64, r, 0);
                let z = self.iconst(Ty::I64, 0);
                self.cmp(CmpOp::Eq, tag, z)
            }
            B::ErrorMsg => {
                let r = self.borrow(&args[0]);
                self.rtv("fl_result_error", &[r])
            }
            B::Unwrap | B::UnwrapOr => {
                let r = self.expr(&args[0]);
                let Type::Result(t) = &args[0].ty else { unreachable!() };
                let t = (**t).clone();
                // The default (if any) is evaluated eagerly, exactly once.
                let dv = if *b == B::UnwrapOr { Some(self.expr(&args[1])) } else { None };
                let res = self.vreg(ir_ty(&t));
                let tag = self.load(Ty::I64, r, 0);
                let z = self.iconst(Ty::I64, 0);
                let is_ok = self.cmp(CmpOp::Eq, tag, z);
                let okb = self.new_block();
                let errb = self.new_block();
                let join = self.new_block();
                self.branch(is_ok, okb, errb);
                self.switch(okb);
                if t != Type::None {
                    let v = self.load(ir_ty(&t), r, 8);
                    self.copy(res, v);
                } else {
                    self.emit(Inst::IConst { dst: res, val: 0 });
                }
                if let Some(d) = dv {
                    if t.is_owned() {
                        self.emit_drop(&t, d);
                    }
                }
                let sz = self.iconst(Ty::I64, 24);
                self.rt("fl_free", &[r, sz]);
                self.jump_to(join);
                self.switch(errb);
                if let Some(d) = dv {
                    self.copy(res, d);
                    let msg = self.load(Ty::I64, r, 16);
                    self.rt("fl_str_drop", &[msg]);
                    let sz = self.iconst(Ty::I64, 24);
                    self.rt("fl_free", &[r, sz]);
                    self.jump_to(join);
                } else {
                    self.rt("fl_unwrap_fail", &[r]);
                    self.term(Term::Unreachable);
                }
                self.switch(join);
                res
            }
            B::ToInt(_) | B::ToFloat => unreachable!(),
        }
    }
}
