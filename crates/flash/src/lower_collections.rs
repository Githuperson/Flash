//! Lowering of Map and Set operations (the hash table itself is written in Flash: runtime/map.flash).

use super::*;

impl<'a, 'p> FnLower<'a, 'p> {
    /// 0 for Int-like and Bool keys, 1 for String keys.
    fn map_kind(&mut self, key: &Type) -> VReg {
        self.iconst(Ty::I64, if matches!(key, Type::Str) { 1 } else { 0 })
    }

    /// A key as the runtime sees it: integers and Bool widened to 64 bits, Strings as pointers.
    pub(super) fn map_key(&mut self, key_ty: &Type, e: &HExpr) -> VReg {
        let v = self.borrow(e);
        self.widen_key(key_ty, v)
    }

    fn widen_key(&mut self, key_ty: &Type, v: VReg) -> VReg {
        match key_ty {
            Type::Int(it) => self.widen(v, *it),
            Type::Bool => self.cast(CastOp::ZExt, v, Ty::I64),
            _ => v,
        }
    }

    fn map_find(&mut self, m: VReg, kind: VReg, key: VReg) -> VReg {
        self.rtv("fl_map_find", &[m, kind, key])
    }

    /// Address of the value slot of entry `e`.
    fn map_val_addr(&mut self, m: VReg, e: VReg) -> VReg {
        let vals = self.load(Ty::I64, m, 32);
        let eight = self.iconst(Ty::I64, 8);
        let off = self.bin(BinOp::Mul, e, eight);
        self.bin(BinOp::Add, vals, off)
    }

    fn map_key_addr(&mut self, m: VReg, e: VReg) -> VReg {
        let keys = self.load(Ty::I64, m, 24);
        let eight = self.iconst(Ty::I64, 8);
        let off = self.bin(BinOp::Mul, e, eight);
        self.bin(BinOp::Add, keys, off)
    }

    /// Run `f(e)` for every live entry number `e`, in insertion order.
    fn map_entries(&mut self, m: VReg, mut f: impl FnMut(&mut Self, VReg)) {
        let n = self.load(Ty::I64, m, 8);
        let e = self.iconst(Ty::I64, 0);
        let head = self.new_block();
        let body = self.new_block();
        let exit = self.new_block();
        self.jump_to(head);
        self.switch(head);
        let c = self.cmp(CmpOp::SLt, e, n);
        self.branch(c, body, exit);
        self.switch(body);
        let live = self.load(Ty::I64, m, 40);
        let addr = self.bin(BinOp::Add, live, e);
        let flag = self.load(Ty::I8, addr, 0);
        let zero = self.iconst(Ty::I8, 0);
        let is_live = self.cmp(CmpOp::Ne, flag, zero);
        let then_b = self.new_block();
        let cont = self.new_block();
        self.branch(is_live, then_b, cont);
        self.switch(then_b);
        f(self, e);
        self.jump_to(cont);
        self.switch(cont);
        let one = self.iconst(Ty::I64, 1);
        let next = self.bin(BinOp::Add, e, one);
        self.copy(e, next);
        self.jump_to(head);
        self.switch(exit);
    }

    /// `m[key] = v`: replace the value of an existing key, or add the key.
    pub(super) fn map_put(&mut self, m: VReg, kt: &Type, vt: &Type, key64: VReg, v: VReg) {
        let kind = self.map_kind(kt);
        let found = self.map_find(m, kind, key64);
        let e = self.vreg(Ty::I64);
        self.copy(e, found);
        let zero = self.iconst(Ty::I64, 0);
        let missing = self.cmp(CmpOp::SLt, found, zero);
        let add_b = self.new_block();
        let have_b = self.new_block();
        let join = self.new_block();
        self.branch(missing, add_b, have_b);
        self.switch(add_b);
        let ne = self.rtv("fl_map_insert", &[m, kind, key64]);
        self.copy(e, ne);
        self.jump_to(join);
        self.switch(have_b);
        if vt.is_owned() {
            let addr = self.map_val_addr(m, e);
            let old = self.load(Ty::I64, addr, 0);
            self.emit_drop(vt, old);
        }
        self.jump_to(join);
        self.switch(join);
        let addr = self.map_val_addr(m, e);
        let te = self.tag_elem(vt);
        self.store_t(addr, 0, v, te);
    }

    /// Address of the value of `m[key]`; a missing key is a runtime error.
    pub(super) fn map_index_addr(&mut self, base: &HExpr, idx: &HExpr) -> VReg {
        let Type::Map(kt, _) = &base.ty else { unreachable!() };
        let m = self.borrow(base);
        let key = self.map_key(kt, idx);
        let kind = self.map_kind(kt);
        let e = self.map_find(m, kind, key);
        let zero = self.iconst(Ty::I64, 0);
        let missing = self.cmp(CmpOp::SLt, e, zero);
        let bad = self.panic_block("fl_panic_key", &[]);
        let ok = self.new_block();
        self.branch(missing, bad, ok);
        self.switch(ok);
        self.map_val_addr(m, e)
    }

    pub(super) fn map_lit(&mut self, pairs: &[(HExpr, HExpr)], ty: &Type) -> VReg {
        let Type::Map(kt, vt) = ty else { unreachable!() };
        let one = self.iconst(Ty::I64, 1);
        let m = self.rtv("fl_map_new", &[one]);
        for (k, v) in pairs {
            let vv = self.expr(v);
            let kk = self.map_key(kt, k);
            self.map_put(m, kt, vt, kk, vv);
        }
        m
    }

    pub(super) fn set_lit(&mut self, elems: &[HExpr], ty: &Type) -> VReg {
        let Type::Set(kt) = ty else { unreachable!() };
        let zero = self.iconst(Ty::I64, 0);
        let m = self.rtv("fl_map_new", &[zero]);
        for e in elems {
            let k = self.map_key(kt, e);
            self.set_add(m, kt, k);
        }
        m
    }

    fn set_add(&mut self, m: VReg, kt: &Type, key64: VReg) {
        let kind = self.map_kind(kt);
        let found = self.map_find(m, kind, key64);
        let zero = self.iconst(Ty::I64, 0);
        let missing = self.cmp(CmpOp::SLt, found, zero);
        self.if_then(missing, |s| {
            s.rt("fl_map_insert", &[m, kind, key64]);
        });
    }

    /// Replace each element of a list (of owned values) by a deep copy.
    fn clone_list_in_place(&mut self, list: VReg, et: &Type) {
        let n = self.load(Ty::I64, list, 0);
        let data = self.load(Ty::I64, list, 16);
        let i = self.iconst(Ty::I64, 0);
        let head = self.new_block();
        let body = self.new_block();
        let exit = self.new_block();
        self.jump_to(head);
        self.switch(head);
        let c = self.cmp(CmpOp::SLt, i, n);
        self.branch(c, body, exit);
        self.switch(body);
        let eight = self.iconst(Ty::I64, 8);
        let off = self.bin(BinOp::Mul, i, eight);
        let addr = self.bin(BinOp::Add, data, off);
        let x = self.load(Ty::I64, addr, 0);
        let c2 = self.emit_clone(et, x);
        self.store(addr, 0, c2);
        let one = self.iconst(Ty::I64, 1);
        let i2 = self.bin(BinOp::Add, i, one);
        self.copy(i, i2);
        self.jump_to(head);
        self.switch(exit);
    }

    /// Release the owned values of a map's live entries.
    fn map_drop_values(&mut self, m: VReg, vt: &Type) {
        if vt.is_owned() {
            let vt = vt.clone();
            self.map_entries(m, move |s, e| {
                let addr = s.map_val_addr(m, e);
                let v = s.load(Ty::I64, addr, 0);
                s.emit_drop(&vt, v);
            });
        }
    }

    pub(super) fn drop_map(&mut self, ty: &Type, p: VReg) {
        let (kt, vt) = match ty {
            Type::Map(k, v) => ((**k).clone(), Some((**v).clone())),
            Type::Set(k) => ((**k).clone(), None),
            _ => unreachable!(),
        };
        if let Some(vt) = &vt {
            self.map_drop_values(p, vt);
        }
        let kind = self.map_kind(&kt);
        self.rt("fl_map_free", &[p, kind]);
    }

    pub(super) fn clone_map(&mut self, ty: &Type, p: VReg) -> VReg {
        let (kt, vt) = match ty {
            Type::Map(k, v) => ((**k).clone(), Some((**v).clone())),
            Type::Set(k) => ((**k).clone(), None),
            _ => unreachable!(),
        };
        let kind = self.map_kind(&kt);
        let new = self.rtv("fl_map_clone_shallow", &[p, kind]);
        if let Some(vt) = vt {
            if vt.is_owned() {
                self.map_entries(new, move |s, e| {
                    let addr = s.map_val_addr(new, e);
                    let v = s.load(Ty::I64, addr, 0);
                    let c = s.emit_clone(&vt, v);
                    s.store(addr, 0, c);
                });
            }
        }
        new
    }

    /// Print `{k: v, ...}` for a Map or `{a, b}` for a Set.
    pub(super) fn print_map(&mut self, ty: &Type, m: VReg) {
        let (kt, vt) = match ty {
            Type::Map(k, v) => ((**k).clone(), Some((**v).clone())),
            Type::Set(k) => ((**k).clone(), None),
            _ => unreachable!(),
        };
        self.print_lit("{");
        let count = self.iconst(Ty::I64, 0);
        self.map_entries(m, |s, e| {
            let zero = s.iconst(Ty::I64, 0);
            let not_first = s.cmp(CmpOp::Ne, count, zero);
            s.if_then(not_first, |s| s.print_lit(", "));
            let kaddr = s.map_key_addr(m, e);
            let k64 = s.load(Ty::I64, kaddr, 0);
            match &kt {
                Type::Int(it) => {
                    s.rt(if it.signed { "fl_print_int" } else { "fl_print_uint" }, &[k64]);
                }
                Type::Bool => {
                    let b = s.cast(CastOp::Trunc, k64, Ty::I8);
                    s.rt("fl_print_bool", &[b]);
                }
                _ => s.print_elem(k64, &Type::Str),
            }
            if let Some(vt) = &vt {
                s.print_lit(": ");
                let vaddr = s.map_val_addr(m, e);
                let v = s.load(ir_ty(vt), vaddr, 0);
                s.print_elem(v, vt);
            }
            let one = s.iconst(Ty::I64, 1);
            let c2 = s.bin(BinOp::Add, count, one);
            s.copy(count, c2);
        });
        self.print_lit("}");
    }

    /// The Map / Set methods (`args[0]` is the receiver).
    pub(super) fn collection_builtin(&mut self, b: &Builtin, args: &[HExpr], e: &HExpr) -> VReg {
        use Builtin as B;
        let (kt, vt) = match &args[0].ty {
            Type::Map(k, v) => ((**k).clone(), Some((**v).clone())),
            Type::Set(k) => ((**k).clone(), None),
            _ => unreachable!(),
        };
        let m = self.borrow(&args[0]);
        let kind = self.map_kind(&kt);
        match b {
            B::MapHas | B::SetHas => {
                let k = self.map_key(&kt, &args[1]);
                let found = self.map_find(m, kind, k);
                let zero = self.iconst(Ty::I64, 0);
                self.cmp(CmpOp::SGe, found, zero)
            }
            B::MapGet => {
                let vt = vt.unwrap();
                let k = self.map_key(&kt, &args[1]);
                let dflt = self.expr(&args[2]);
                let found = self.map_find(m, kind, k);
                let res = self.vreg(ir_ty(&vt));
                let zero = self.iconst(Ty::I64, 0);
                let hit = self.cmp(CmpOp::SGe, found, zero);
                let yes = self.new_block();
                let no = self.new_block();
                let join = self.new_block();
                self.branch(hit, yes, no);
                self.switch(yes);
                let addr = self.map_val_addr(m, found);
                let te = self.tag_elem(&vt);
                let mut v = self.load_t(ir_ty(&vt), addr, 0, te);
                if vt.is_owned() {
                    v = self.emit_clone(&vt, v);
                    self.emit_drop(&vt, dflt);
                }
                self.copy(res, v);
                self.jump_to(join);
                self.switch(no);
                self.copy(res, dflt);
                self.jump_to(join);
                self.switch(join);
                res
            }
            B::MapRemove | B::SetRemove => {
                let k = self.map_key(&kt, &args[1]);
                let found = self.map_find(m, kind, k);
                let zero = self.iconst(Ty::I64, 0);
                let hit = self.cmp(CmpOp::SGe, found, zero);
                let vt2 = vt.clone();
                self.if_then(hit, move |s| {
                    if let Some(vt) = &vt2 {
                        if vt.is_owned() {
                            let addr = s.map_val_addr(m, found);
                            let old = s.load(Ty::I64, addr, 0);
                            s.emit_drop(vt, old);
                        }
                    }
                    s.rt("fl_map_remove", &[m, kind, found]);
                });
                self.dummy()
            }
            B::MapClear | B::SetClear => {
                if let Some(vt) = &vt {
                    self.map_drop_values(m, vt);
                }
                self.rt("fl_map_clear", &[m, kind]);
                self.dummy()
            }
            B::SetAdd => {
                let k = self.map_key(&kt, &args[1]);
                self.set_add(m, &kt, k);
                self.dummy()
            }
            B::MapKeys | B::SetItems => {
                let stride = self.iconst(Ty::I64, kt.size() as i64);
                self.rtv("fl_map_keys", &[m, kind, stride])
            }
            B::MapValues => {
                let vt = vt.unwrap();
                let stride = self.iconst(Ty::I64, vt.size() as i64);
                let l = self.rtv("fl_map_values", &[m, stride]);
                if vt.is_owned() {
                    self.clone_list_in_place(l, &vt);
                }
                let _ = e;
                l
            }
            _ => unreachable!(),
        }
    }
}
