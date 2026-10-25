//! HIR → IR lowering. Makes ownership explicit: drops of owned locals at scope exits,
//! drop flags for conditionally moved variables, temporaries, bounds checks, and
//! drop/clone glue per type.

use std::collections::{HashMap, HashSet};

use crate::ast::{BinOp as AB, UnOp as AU};
use crate::hir::*;
use crate::ir::*;
use crate::rtsigs;
use crate::types::{IntTy, StructInfo, Type};

/// Name and size of the runtime library's global state block.
pub const RT_STATE_NAME: &str = "__rt_state";
pub const RT_STATE_BYTES: usize = 65536;

pub fn ir_ty(t: &Type) -> Ty {
    match t {
        Type::Int(i) => match i.bits {
            8 => Ty::I8,
            16 => Ty::I16,
            32 => Ty::I32,
            _ => Ty::I64,
        },
        Type::Float => Ty::F64,
        Type::Bool | Type::None | Type::Error => Ty::I8,
        _ => Ty::I64,
    }
}

fn int_ty_of(t: &Type) -> IntTy {
    match t {
        Type::Int(i) => *i,
        _ => crate::types::INT,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum GlueKind {
    Drop,
    Clone,
}

pub struct Lowerer<'p> {
    prog: &'p Program,
    pub m: Module,
    strs: HashMap<String, DataId>,
    glue: HashMap<(GlueKind, Type), FuncRef>,
    tags: HashMap<String, MemTag>,
    /// Name functions exactly as written (the runtime library, whose `fl_*` functions are looked up by name).
    keep_names: bool,
    /// Value of the runtime's `__os()` constant: 0 = Linux, 1 = macOS.
    pub target_os: i64,
}

pub fn lower(prog: &Program) -> Module {
    lower_with(prog, false, 0)
}

/// Lower the runtime library for an operating system: functions keep their source names.
pub fn lower_runtime(prog: &Program, target_os: i64) -> Module {
    lower_with(prog, true, target_os)
}

fn lower_with(prog: &Program, keep_names: bool, target_os: i64) -> Module {
    let mut lw = Lowerer { prog, m: Module::default(), strs: HashMap::new(), glue: HashMap::new(), tags: HashMap::new(), keep_names, target_os };
    lw.m.tags.push(String::new());
    // Reserve function slots so calls can refer to any function by index.
    for f in &prog.funcs {
        lw.m.funcs.push(Function {
            name: String::new(),
            params: vec![],
            ret: None,
            vregs: vec![],
            blocks: vec![],
            stack_slots: vec![],
            exported: false,
            no_inline: false,
        });
        let _ = f;
    }
    for (i, f) in prog.funcs.iter().enumerate() {
        let func = FnLower::lower_func(&mut lw, f, i);
        lw.m.funcs[i] = func;
    }
    lw.m
}

impl<'p> Lowerer<'p> {
    fn string_data(&mut self, s: &str) -> DataId {
        if let Some(d) = self.strs.get(s) {
            return *d;
        }
        let mut bytes = Vec::with_capacity(16 + s.len());
        bytes.extend_from_slice(&(s.len() as i64).to_le_bytes());
        bytes.extend_from_slice(&(-1i64).to_le_bytes());
        bytes.extend_from_slice(s.as_bytes());
        let id = self.m.data.len() as u32;
        self.m.data.push(DataItem { name: format!(".Lstr{}", id), bytes, align: 8, writable: false });
        self.strs.insert(s.to_string(), id);
        id
    }

    /// The zero-initialised, writable block the runtime library keeps its global state in.
    pub fn state_data(&mut self) -> DataId {
        if let Some(id) = self.m.data.iter().position(|d| d.name == RT_STATE_NAME) {
            return id as u32;
        }
        self.m.data.push(DataItem { name: RT_STATE_NAME.into(), bytes: vec![0; RT_STATE_BYTES], align: 4096, writable: true });
        (self.m.data.len() - 1) as u32
    }

    pub fn raw_data(&mut self, name: &str, bytes: Vec<u8>) -> DataId {
        let id = self.m.data.len() as u32;
        self.m.data.push(DataItem { name: format!(".L{}{}", name, id), bytes, align: 8, writable: false });
        id
    }

    pub fn tag(&mut self, name: &str) -> MemTag {
        if let Some(t) = self.tags.get(name) {
            return *t;
        }
        let id = self.m.tags.len() as MemTag;
        self.m.tags.push(name.to_string());
        self.tags.insert(name.to_string(), id);
        id
    }

    fn ext(&mut self, name: &str) -> ExternRef {
        let (p, r) = rtsigs::lookup(name).unwrap_or_else(|| panic!("unknown runtime function {}", name));
        self.m.extern_ref(name, p, r)
    }

    fn structs(&self) -> &'p [StructInfo] {
        &self.prog.structs
    }

    /// Get (creating on demand) the drop or clone glue function for an owned type.
    fn glue(&mut self, kind: GlueKind, ty: &Type) -> FuncRef {
        if let Some(f) = self.glue.get(&(kind, ty.clone())) {
            return *f;
        }
        let idx = self.m.funcs.len() as u32;
        self.m.funcs.push(Function {
            name: format!("{}_{}", if kind == GlueKind::Drop { "drop" } else { "clone" }, idx),
            params: vec![Ty::I64],
            ret: if kind == GlueKind::Clone { Some(Ty::I64) } else { None },
            vregs: vec![Ty::I64],
            blocks: vec![],
            stack_slots: vec![],
            exported: false,
            no_inline: false,
        });
        self.glue.insert((kind, ty.clone()), idx);
        let f = FnLower::lower_glue(self, kind, ty, idx);
        self.m.funcs[idx as usize] = f;
        idx
    }
}

struct Scope {
    locals: Vec<LocalId>,
}

struct LoopCtx {
    break_bb: BlockId,
    continue_bb: BlockId,
    scope_depth: usize,
}

struct FnLower<'a, 'p> {
    lw: &'a mut Lowerer<'p>,
    f: Function,
    cur: BlockId,
    local_regs: Vec<VReg>,
    local_tys: Vec<Type>,
    flags: HashMap<LocalId, VReg>,
    scopes: Vec<Scope>,
    loops: Vec<LoopCtx>,
    temps: Vec<Vec<(VReg, Type)>>,
    ret_ty: Type,
    is_main: bool,
    hf_locals: Vec<LocalInfo>,
    /// (list variable, index variable) pairs for which `list[index]` is proven in range.
    safe_index: Vec<(LocalId, LocalId)>,
}

/// Does any statement in `stmts` possibly change the length of (or move / replace) list `xs`?
fn body_mutates(stmts: &[HStmt], xs: LocalId) -> bool {
    fn root(e: &HExpr) -> Option<LocalId> {
        match &e.kind {
            HKind::Local(id, _) => Some(*id),
            HKind::Field(b, _) | HKind::Index(b, _) => root(b),
            _ => None,
        }
    }
    fn expr(e: &HExpr, xs: LocalId) -> bool {
        match &e.kind {
            HKind::Local(id, UseMode::Move) => *id == xs,
            HKind::Builtin(b, args) => {
                (matches!(b, Builtin::Append | Builtin::Pop) && args.first().and_then(root) == Some(xs)) || args.iter().any(|a| expr(a, xs))
            }
            HKind::Call(_, args) => args.iter().any(|a| root(a) == Some(xs) && !a.ty.is_copy() || expr(a, xs)),
            HKind::PluginCall { args, .. } | HKind::ExternCall { args, .. } | HKind::Struct(args) | HKind::List(args) => {
                args.iter().any(|a| expr(a, xs))
            }
            HKind::Unary(_, a) | HKind::Cast(a) | HKind::Try(a) | HKind::Field(a, _) => expr(a, xs),
            HKind::Binary(_, a, b) | HKind::Logic(_, a, b) | HKind::Index(a, b) => expr(a, xs) || expr(b, xs),
            _ => false,
        }
    }
    stmts.iter().any(|s| match &s.kind {
        HStmtKind::Let(_, e) | HStmtKind::Expr(e) => expr(e, xs),
        HStmtKind::Assign(p, v) => root(p) == Some(xs) || expr(p, xs) || expr(v, xs),
        HStmtKind::If(c, a, b) => expr(c, xs) || body_mutates(a, xs) || body_mutates(b, xs),
        HStmtKind::While(c, b) => expr(c, xs) || body_mutates(b, xs),
        HStmtKind::ForRange { lo, hi, body, .. } => expr(lo, xs) || expr(hi, xs) || body_mutates(body, xs),
        HStmtKind::ForList { list, body, .. } => expr(list, xs) || body_mutates(body, xs),
        HStmtKind::Return(Some(e)) => expr(e, xs),
        _ => false,
    })
}

/// Is local `v` assigned anywhere in `stmts`?
fn body_assigns(stmts: &[HStmt], v: LocalId) -> bool {
    stmts.iter().any(|s| match &s.kind {
        HStmtKind::Assign(p, _) => matches!(p.kind, HKind::Local(id, _) if id == v),
        HStmtKind::If(_, a, b) => body_assigns(a, v) || body_assigns(b, v),
        HStmtKind::While(_, b) | HStmtKind::ForRange { body: b, .. } | HStmtKind::ForList { body: b, .. } => body_assigns(b, v),
        _ => false,
    })
}

fn collect_moves_expr(e: &HExpr, out: &mut HashSet<LocalId>) {
    match &e.kind {
        HKind::Local(id, UseMode::Move) => {
            out.insert(*id);
        }
        HKind::Unary(_, a) | HKind::Field(a, _) | HKind::Cast(a) | HKind::Try(a) => collect_moves_expr(a, out),
        HKind::Binary(_, a, b) | HKind::Logic(_, a, b) | HKind::Index(a, b) => {
            collect_moves_expr(a, out);
            collect_moves_expr(b, out);
        }
        HKind::Call(_, args)
        | HKind::Builtin(_, args)
        | HKind::Struct(args)
        | HKind::List(args)
        | HKind::PluginCall { args, .. }
        | HKind::ExternCall { args, .. } => {
            for a in args {
                collect_moves_expr(a, out);
            }
        }
        _ => {}
    }
}

fn collect_moves(stmts: &[HStmt], out: &mut HashSet<LocalId>) {
    for s in stmts {
        match &s.kind {
            HStmtKind::Let(_, e) | HStmtKind::Expr(e) => collect_moves_expr(e, out),
            HStmtKind::Assign(p, v) => {
                collect_moves_expr(p, out);
                collect_moves_expr(v, out);
            }
            HStmtKind::If(c, a, b) => {
                collect_moves_expr(c, out);
                collect_moves(a, out);
                collect_moves(b, out);
            }
            HStmtKind::While(c, b) => {
                collect_moves_expr(c, out);
                collect_moves(b, out);
            }
            HStmtKind::ForRange { lo, hi, body, .. } => {
                collect_moves_expr(lo, out);
                collect_moves_expr(hi, out);
                collect_moves(body, out);
            }
            HStmtKind::ForList { list, body, .. } => {
                collect_moves_expr(list, out);
                collect_moves(body, out);
            }
            HStmtKind::Return(Some(e)) => collect_moves_expr(e, out),
            _ => {}
        }
    }
}

impl<'a, 'p> FnLower<'a, 'p> {
    fn new(lw: &'a mut Lowerer<'p>, name: String, params: Vec<Ty>, ret: Option<Ty>) -> FnLower<'a, 'p> {
        let mut f = Function {
            name,
            params: params.clone(),
            ret,
            vregs: params,
            blocks: vec![Block { insts: vec![], term: Term::Unreachable }],
            stack_slots: vec![],
            exported: false,
            no_inline: false,
        };
        let _ = &mut f;
        FnLower {
            lw,
            f,
            cur: 0,
            local_regs: vec![],
            local_tys: vec![],
            flags: HashMap::new(),
            scopes: vec![],
            loops: vec![],
            temps: vec![],
            ret_ty: Type::None,
            is_main: false,
            hf_locals: vec![],
            safe_index: vec![],
        }
    }

    // ------------------------------------------------------------ emit helpers
    fn vreg(&mut self, ty: Ty) -> VReg {
        self.f.new_vreg(ty)
    }
    fn new_block(&mut self) -> BlockId {
        self.f.blocks.push(Block { insts: vec![], term: Term::Unreachable });
        (self.f.blocks.len() - 1) as u32
    }
    fn emit(&mut self, i: Inst) {
        self.f.blocks[self.cur as usize].insts.push(i);
    }
    fn term(&mut self, t: Term) {
        self.f.blocks[self.cur as usize].term = t;
    }
    fn switch(&mut self, b: BlockId) {
        self.cur = b;
    }
    fn jump_to(&mut self, b: BlockId) {
        self.term(Term::Jump(b));
    }
    fn iconst(&mut self, ty: Ty, val: i64) -> VReg {
        let dst = self.vreg(ty);
        self.emit(Inst::IConst { dst, val });
        dst
    }
    fn fconst(&mut self, val: f64) -> VReg {
        let dst = self.vreg(Ty::F64);
        self.emit(Inst::FConst { dst, val });
        dst
    }
    fn bin(&mut self, op: BinOp, a: VReg, b: VReg) -> VReg {
        let ty = self.f.vregs[a as usize];
        let dst = self.vreg(ty);
        self.emit(Inst::Bin { dst, op, a, b });
        dst
    }
    fn cmp(&mut self, op: CmpOp, a: VReg, b: VReg) -> VReg {
        let dst = self.vreg(Ty::I8);
        self.emit(Inst::Cmp { dst, op, a, b });
        dst
    }
    fn load(&mut self, ty: Ty, base: VReg, off: i32) -> VReg {
        self.load_t(ty, base, off, NO_TAG)
    }
    fn store(&mut self, base: VReg, off: i32, val: VReg) {
        self.store_t(base, off, val, NO_TAG)
    }
    fn load_t(&mut self, ty: Ty, base: VReg, off: i32, tag: MemTag) -> VReg {
        let dst = self.vreg(ty);
        self.emit(Inst::Load { dst, base, off, tag });
        dst
    }
    fn store_t(&mut self, base: VReg, off: i32, val: VReg, tag: MemTag) {
        self.emit(Inst::Store { base, off, val, tag });
    }
    /// Alias class for list/string length (offset 0) and list data pointer (offset 16).
    fn tag_len(&mut self) -> MemTag {
        self.lw.tag("hdr.len")
    }
    fn tag_data(&mut self) -> MemTag {
        self.lw.tag("list.data")
    }
    /// List elements are classed by machine type (a `Float` slot never aliases an `Int` slot).
    fn tag_elem(&mut self, ty: &Type) -> MemTag {
        let name = match ir_ty(ty) {
            Ty::F64 => "elem.f64",
            Ty::I64 => "elem.i64",
            Ty::I32 => "elem.i32",
            Ty::I16 => "elem.i16",
            Ty::I8 => "elem.i8",
        };
        self.lw.tag(name)
    }
    fn tag_field(&mut self, sid: usize, idx: usize) -> MemTag {
        self.lw.tag(&format!("field.{}.{}", sid, idx))
    }
    fn copy(&mut self, dst: VReg, src: VReg) {
        self.emit(Inst::Copy { dst, src });
    }
    fn cast(&mut self, op: CastOp, src: VReg, to: Ty) -> VReg {
        let dst = self.vreg(to);
        self.emit(Inst::Cast { dst, op, src });
        dst
    }
    fn rt(&mut self, name: &str, args: &[VReg]) -> Option<VReg> {
        let (_, ret) = rtsigs::lookup(name).unwrap_or_else(|| panic!("unknown runtime function {}", name));
        let e = self.lw.ext(name);
        let dst = ret.map(|t| self.vreg(t));
        self.emit(Inst::Call { dst, callee: Callee::Extern(e), args: args.to_vec() });
        dst
    }
    fn rtv(&mut self, name: &str, args: &[VReg]) -> VReg {
        self.rt(name, args).unwrap()
    }
    fn dummy(&mut self) -> VReg {
        self.iconst(Ty::I8, 0)
    }
    fn branch(&mut self, cond: VReg, t: BlockId, e: BlockId) {
        self.term(Term::Br { cond, then_: t, else_: e });
    }
    /// Emit `if cond { body }` where `body` runs in its own block.
    fn if_then(&mut self, cond: VReg, body: impl FnOnce(&mut Self)) {
        let t = self.new_block();
        let j = self.new_block();
        self.branch(cond, t, j);
        self.switch(t);
        body(self);
        self.jump_to(j);
        self.switch(j);
    }
    fn panic_block(&mut self, name: &str, args: &[VReg]) -> BlockId {
        let saved = self.cur;
        let b = self.new_block();
        self.switch(b);
        self.rt(name, args);
        self.term(Term::Unreachable);
        self.switch(saved);
        b
    }

    // -------------------------------------------------------------- functions
    fn lower_func(lw: &'a mut Lowerer<'p>, hf: &'p HFunc, _idx: usize) -> Function {
        let params: Vec<Ty> = hf.params.iter().map(|p| ir_ty(&hf.locals[*p].ty)).collect();
        let ret = if hf.ret == Type::None { None } else { Some(ir_ty(&hf.ret)) };
        let name = if hf.is_main && lw.keep_names {
            "__rt_main".to_string() // the (empty) script of the runtime library itself
        } else if hf.is_main {
            "flash_main".to_string()
        } else if lw.keep_names {
            hf.name.clone()
        } else {
            format!("{}_{}", hf.name, _idx)
        };
        let mut fl = FnLower::new(lw, name, params, ret);
        fl.f.exported = hf.is_main;
        fl.ret_ty = hf.ret.clone();
        fl.is_main = hf.is_main;
        fl.hf_locals = hf.locals.clone();
        let mut moved = HashSet::new();
        collect_moves(&hf.body, &mut moved);
        // Allocate registers for locals; parameters live in the first vregs.
        for (i, l) in hf.locals.iter().enumerate() {
            let ty = ir_ty(&l.ty);
            let r = match hf.params.iter().position(|p| *p == i) {
                Some(pi) => pi as VReg,
                None => fl.vreg(ty),
            };
            fl.local_regs.push(r);
            fl.local_tys.push(l.ty.clone());
            if l.kind.owns() && l.ty.is_owned() && moved.contains(&i) {
                let fr = fl.vreg(Ty::I8);
                fl.flags.insert(i, fr);
                fl.emit(Inst::IConst { dst: fr, val: 0 });
            }
        }
        fl.scopes.push(Scope { locals: vec![] });
        if hf.is_main {
            fl.emit_plugin_loads();
        }
        for p in &hf.params {
            let l = &hf.locals[*p];
            if l.kind.owns() && l.ty.is_owned() {
                fl.scopes[0].locals.push(*p);
                if let Some(fr) = fl.flags.get(p).copied() {
                    fl.emit(Inst::IConst { dst: fr, val: 1 });
                }
            }
        }
        fl.stmts(&hf.body);
        // Fall off the end: drop everything and return.
        fl.drop_scopes_to(0);
        fl.term(Term::Ret(None).clone());
        if fl.f.ret.is_some() {
            // Verified by the checker: unreachable at run time.
            fl.term(Term::Unreachable);
        }
        fl.f
    }

    fn lower_glue(lw: &'a mut Lowerer<'p>, kind: GlueKind, ty: &Type, idx: FuncRef) -> Function {
        let name = format!("{}_{}", if kind == GlueKind::Drop { "drop" } else { "clone" }, idx);
        let ret = if kind == GlueKind::Clone { Some(Ty::I64) } else { None };
        let mut fl = FnLower::new(lw, name, vec![Ty::I64], ret);
        let p: VReg = 0;
        match kind {
            GlueKind::Drop => {
                fl.drop_body(ty, p);
                fl.term(Term::Ret(None));
            }
            GlueKind::Clone => {
                let r = fl.clone_body(ty, p);
                fl.term(Term::Ret(Some(r)));
            }
        }
        fl.f.no_inline = false;
        fl.f
    }

    fn stride(ty: &Type) -> i64 {
        ty.size() as i64
    }

    /// Release the heap memory behind an owned value (the *body* of drop glue).
    fn drop_body(&mut self, ty: &Type, p: VReg) {
        match ty {
            Type::Str => {
                self.rt("fl_str_drop", &[p]);
            }
            Type::List(et) => {
                if et.is_owned() {
                    let n = self.load(Ty::I64, p, 0);
                    let data = self.load(Ty::I64, p, 16);
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
                    let e = self.load(Ty::I64, addr, 0);
                    self.emit_drop(et, e);
                    let one = self.iconst(Ty::I64, 1);
                    let i2 = self.bin(BinOp::Add, i, one);
                    self.copy(i, i2);
                    self.jump_to(head);
                    self.switch(exit);
                }
                let stride = self.iconst(Ty::I64, Self::stride(et));
                self.rt("fl_list_free", &[p, stride]);
            }
            Type::Struct(sid) => {
                let si = &self.lw.structs()[*sid];
                for (i, (_, ft)) in si.fields.iter().enumerate() {
                    if ft.is_owned() {
                        let v = self.load(Ty::I64, p, si.offsets[i] as i32);
                        self.emit_drop(ft, v);
                    }
                }
                let sz = self.iconst(Ty::I64, si.size as i64);
                self.rt("fl_free", &[p, sz]);
            }
            Type::Result(t) => {
                let tag = self.load(Ty::I64, p, 0);
                let zero = self.iconst(Ty::I64, 0);
                let is_ok = self.cmp(CmpOp::Eq, tag, zero);
                let okb = self.new_block();
                let errb = self.new_block();
                let join = self.new_block();
                self.branch(is_ok, okb, errb);
                self.switch(okb);
                if t.is_owned() {
                    let v = self.load(Ty::I64, p, 8);
                    self.emit_drop(t, v);
                }
                self.jump_to(join);
                self.switch(errb);
                let s = self.load(Ty::I64, p, 16);
                self.rt("fl_str_drop", &[s]);
                self.jump_to(join);
                self.switch(join);
                let sz = self.iconst(Ty::I64, 24);
                self.rt("fl_free", &[p, sz]);
            }
            _ => {}
        }
    }

    fn emit_drop(&mut self, ty: &Type, v: VReg) {
        match ty {
            Type::Str => {
                self.rt("fl_str_drop", &[v]);
            }
            Type::List(et) if !et.is_owned() => {
                let stride = self.iconst(Ty::I64, Self::stride(et));
                self.rt("fl_list_free", &[v, stride]);
            }
            t if t.is_owned() => {
                let g = self.lw.glue(GlueKind::Drop, t);
                self.emit(Inst::Call { dst: None, callee: Callee::Func(g), args: vec![v] });
            }
            _ => {}
        }
    }

    fn emit_clone(&mut self, ty: &Type, v: VReg) -> VReg {
        match ty {
            Type::Str => self.rtv("fl_str_clone", &[v]),
            t if t.is_owned() => {
                let g = self.lw.glue(GlueKind::Clone, t);
                let dst = self.vreg(Ty::I64);
                self.emit(Inst::Call { dst: Some(dst), callee: Callee::Func(g), args: vec![v] });
                dst
            }
            _ => v,
        }
    }

    fn clone_body(&mut self, ty: &Type, p: VReg) -> VReg {
        match ty {
            Type::List(et) => {
                let stride = self.iconst(Ty::I64, Self::stride(et));
                let new = self.rtv("fl_list_clone_shallow", &[p, stride]);
                if et.is_owned() {
                    let n = self.load(Ty::I64, new, 0);
                    let data = self.load(Ty::I64, new, 16);
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
                    let e = self.load(Ty::I64, addr, 0);
                    let c2 = self.emit_clone(et, e);
                    self.store(addr, 0, c2);
                    let one = self.iconst(Ty::I64, 1);
                    let i2 = self.bin(BinOp::Add, i, one);
                    self.copy(i, i2);
                    self.jump_to(head);
                    self.switch(exit);
                }
                new
            }
            Type::Struct(sid) => {
                let si = &self.lw.structs()[*sid];
                let sz = self.iconst(Ty::I64, si.size as i64);
                let new = self.rtv("fl_alloc", &[sz]);
                for (i, (_, ft)) in si.fields.iter().enumerate() {
                    let off = si.offsets[i] as i32;
                    let v = self.load(ir_ty(ft), p, off);
                    let v2 = if ft.is_owned() { self.emit_clone(ft, v) } else { v };
                    self.store(new, off, v2);
                }
                new
            }
            _ => unreachable!("no clone glue for {:?}", ty),
        }
    }

    // --------------------------------------------------------------- scopes
    fn drop_local(&mut self, id: LocalId) {
        let ty = self.local_tys[id].clone();
        let r = self.local_regs[id];
        if let Some(fl) = self.flags.get(&id).copied() {
            self.if_then(fl, |s| s.emit_drop(&ty, r));
        } else {
            self.emit_drop(&ty, r);
        }
    }

    /// Emit drops for all scopes above `depth` without popping them (for break/return).
    fn drop_scopes_to(&mut self, depth: usize) {
        for si in (depth..self.scopes.len()).rev() {
            let locals = self.scopes[si].locals.clone();
            for id in locals.iter().rev() {
                self.drop_local(*id);
            }
        }
    }

    fn pending_temps_drop(&mut self) {
        let all: Vec<(VReg, Type)> = self.temps.iter().flatten().cloned().collect();
        for (v, t) in all.into_iter().rev() {
            self.emit_drop(&t, v);
        }
    }

    fn begin_temps(&mut self) {
        self.temps.push(vec![]);
    }
    fn end_temps(&mut self) {
        let ts = self.temps.pop().unwrap();
        for (v, t) in ts.into_iter().rev() {
            self.emit_drop(&t, v);
        }
    }

    fn block(&mut self, stmts: &[HStmt]) {
        self.scopes.push(Scope { locals: vec![] });
        self.stmts(stmts);
        let sc = self.scopes.pop().unwrap();
        for id in sc.locals.iter().rev() {
            self.drop_local(*id);
        }
    }

    fn stmts(&mut self, stmts: &[HStmt]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    /// After a terminator, continue emitting into a fresh (unreachable) block.
    fn start_dead_block(&mut self) {
        let b = self.new_block();
        self.switch(b);
    }

    fn stmt(&mut self, s: &HStmt) {
        match &s.kind {
            HStmtKind::Let(id, e) => {
                self.begin_temps();
                let v = self.expr(e);
                self.end_temps();
                let r = self.local_regs[*id];
                self.copy(r, v);
                if self.local_tys[*id].is_owned() && self.hf_locals[*id].kind.owns() {
                    self.scopes.last_mut().unwrap().locals.push(*id);
                    if let Some(fr) = self.flags.get(id).copied() {
                        self.emit(Inst::IConst { dst: fr, val: 1 });
                    }
                }
            }
            HStmtKind::Assign(place, value) => {
                self.begin_temps();
                let v = self.expr(value);
                self.assign_place(place, v);
                self.end_temps();
            }
            HStmtKind::Expr(e) => {
                self.begin_temps();
                let v = self.expr(e);
                if e.ty.is_owned() {
                    self.emit_drop(&e.ty, v);
                }
                self.end_temps();
            }
            HStmtKind::If(c, a, b) => {
                let cv = self.cond(c);
                let tb = self.new_block();
                let eb = self.new_block();
                let jb = self.new_block();
                self.branch(cv, tb, eb);
                self.switch(tb);
                self.block(a);
                self.jump_to(jb);
                self.switch(eb);
                self.block(b);
                self.jump_to(jb);
                self.switch(jb);
            }
            HStmtKind::While(c, body) => {
                let head = self.new_block();
                let bb = self.new_block();
                let exit = self.new_block();
                self.jump_to(head);
                self.switch(head);
                let cv = self.cond(c);
                self.branch(cv, bb, exit);
                self.switch(bb);
                self.loops.push(LoopCtx { break_bb: exit, continue_bb: head, scope_depth: self.scopes.len() });
                self.block(body);
                self.loops.pop();
                self.jump_to(head);
                self.switch(exit);
            }
            HStmtKind::ForRange { var, lo, hi, inclusive, body } => self.for_range(*var, lo, hi, *inclusive, body),
            HStmtKind::ForList { var, list, body } => self.for_list(*var, list, body),
            HStmtKind::Break => {
                let lc = self.loops.last().expect("break outside loop");
                let (bb, depth) = (lc.break_bb, lc.scope_depth);
                self.drop_scopes_to(depth);
                self.jump_to(bb);
                self.start_dead_block();
            }
            HStmtKind::Continue => {
                let lc = self.loops.last().expect("continue outside loop");
                let (bb, depth) = (lc.continue_bb, lc.scope_depth);
                self.drop_scopes_to(depth);
                self.jump_to(bb);
                self.start_dead_block();
            }
            HStmtKind::Return(e) => {
                self.begin_temps();
                let v = e.as_ref().map(|e| self.expr(e));
                self.end_temps();
                self.drop_scopes_to(0);
                if self.ret_ty == Type::None {
                    self.term(Term::Ret(None));
                } else {
                    self.term(Term::Ret(v));
                }
                self.start_dead_block();
            }
        }
    }

    fn cond(&mut self, c: &HExpr) -> VReg {
        self.begin_temps();
        let v = self.expr(c);
        self.end_temps();
        v
    }

    fn for_range(&mut self, var: LocalId, lo: &HExpr, hi: &HExpr, inclusive: bool, body: &[HStmt]) {
        self.begin_temps();
        let lov = self.expr(lo);
        let hiv = self.expr(hi);
        self.end_temps();
        let it = int_ty_of(&lo.ty);
        let ty = ir_ty(&lo.ty);
        let cnt = self.vreg(ty);
        self.copy(cnt, lov);
        let hi_r = self.vreg(ty);
        self.copy(hi_r, hiv);
        let (lt, le) = if it.signed { (CmpOp::SLt, CmpOp::SLe) } else { (CmpOp::ULt, CmpOp::ULe) };
        let head = self.new_block();
        let bb = self.new_block();
        let latch = self.new_block();
        let exit = self.new_block();
        let vr = self.local_regs[var];
        // Bounds-check elimination: `for i in 0..len(xs)` where xs is not modified in the body
        // and i is not reassigned proves `xs[i]` is in range.
        let mut proven: Option<(LocalId, LocalId)> = None;
        if !inclusive {
            if let (HKind::Int(l), HKind::Builtin(Builtin::Len, a)) = (&lo.kind, &hi.kind) {
                if *l >= 0 {
                    if let HKind::Local(xs, _) = a[0].kind {
                        if matches!(a[0].ty, Type::List(_)) && !body_mutates(body, xs) && !body_assigns(body, var) {
                            proven = Some((xs, var));
                        }
                    }
                }
            }
        }
        if let Some(p) = proven {
            self.safe_index.push(p);
        }
        if inclusive {
            let ok = self.cmp(le, cnt, hi_r);
            self.branch(ok, bb, exit);
        } else {
            self.jump_to(head);
            self.switch(head);
            let ok = self.cmp(lt, cnt, hi_r);
            self.branch(ok, bb, exit);
        }
        self.switch(bb);
        self.copy(vr, cnt);
        self.loops.push(LoopCtx { break_bb: exit, continue_bb: latch, scope_depth: self.scopes.len() });
        self.block(body);
        self.loops.pop();
        if proven.is_some() {
            self.safe_index.pop();
        }
        self.jump_to(latch);
        self.switch(latch);
        if inclusive {
            // stop when cnt == hi, so hi == MAX does not overflow
            let done = self.cmp(CmpOp::Eq, cnt, hi_r);
            let inc = self.new_block();
            self.branch(done, exit, inc);
            self.switch(inc);
            let one = self.iconst(ty, 1);
            let n = self.bin(BinOp::Add, cnt, one);
            self.copy(cnt, n);
            self.jump_to(bb);
        } else {
            let one = self.iconst(ty, 1);
            let n = self.bin(BinOp::Add, cnt, one);
            self.copy(cnt, n);
            self.jump_to(head);
        }
        self.switch(exit);
    }

    fn for_list(&mut self, var: LocalId, list: &HExpr, body: &[HStmt]) {
        let Type::List(et) = &list.ty else { unreachable!() };
        let et = (**et).clone();
        self.begin_temps();
        let lp = self.expr(list);
        self.end_temps();
        let tl = self.tag_len();
        let n = self.load_t(Ty::I64, lp, 0, tl);
        let idx = self.iconst(Ty::I64, 0);
        let head = self.new_block();
        let bb = self.new_block();
        let latch = self.new_block();
        let exit = self.new_block();
        self.jump_to(head);
        self.switch(head);
        let c = self.cmp(CmpOp::SLt, idx, n);
        self.branch(c, bb, exit);
        self.switch(bb);
        let td = self.tag_data();
        let data = self.load_t(Ty::I64, lp, 16, td);
        let stride = self.iconst(Ty::I64, Self::stride(&et));
        let off = self.bin(BinOp::Mul, idx, stride);
        let addr = self.bin(BinOp::Add, data, off);
        let te = self.tag_elem(&et);
        let ev = self.load_t(ir_ty(&et), addr, 0, te);
        let vr = self.local_regs[var];
        self.copy(vr, ev);
        self.loops.push(LoopCtx { break_bb: exit, continue_bb: latch, scope_depth: self.scopes.len() });
        self.block(body);
        self.loops.pop();
        self.jump_to(latch);
        self.switch(latch);
        let one = self.iconst(Ty::I64, 1);
        let n2 = self.bin(BinOp::Add, idx, one);
        self.copy(idx, n2);
        self.jump_to(head);
        self.switch(exit);
    }

    // ------------------------------------------------------------- places
    fn assign_place(&mut self, place: &HExpr, v: VReg) {
        match &place.kind {
            HKind::Local(id, _) => {
                let ty = self.local_tys[*id].clone();
                let r = self.local_regs[*id];
                if ty.is_owned() && self.hf_locals[*id].kind.owns() {
                    // drop the old value (if still owned), then store
                    self.drop_local(*id);
                    self.copy(r, v);
                    if let Some(fr) = self.flags.get(id).copied() {
                        self.emit(Inst::IConst { dst: fr, val: 1 });
                    }
                } else {
                    self.copy(r, v);
                }
            }
            HKind::Field(base, idx) => {
                let b = self.borrow(base);
                let Type::Struct(sid) = &base.ty else { unreachable!() };
                let si = &self.lw.structs()[*sid];
                let off = si.offsets[*idx] as i32;
                let fty = si.fields[*idx].1.clone();
                let tf = self.tag_field(*sid, *idx);
                if fty.is_owned() {
                    let old = self.load_t(Ty::I64, b, off, tf);
                    self.emit_drop(&fty, old);
                }
                self.store_t(b, off, v, tf);
            }
            HKind::Index(base, idx) => {
                let addr = self.elem_addr(base, idx);
                let te = self.tag_elem(&place.ty);
                if place.ty.is_owned() {
                    let old = self.load_t(Ty::I64, addr, 0, te);
                    self.emit_drop(&place.ty, old);
                }
                self.store_t(addr, 0, v, te);
            }
            _ => unreachable!("not a place"),
        }
    }

    /// Bounds-checked address of `base[idx]`.
    fn elem_addr(&mut self, base: &HExpr, idx: &HExpr) -> VReg {
        let Type::List(et) = &base.ty else { unreachable!() };
        let b = self.borrow(base);
        let i = self.expr(idx);
        let it = int_ty_of(&idx.ty);
        let i64v = self.widen(i, it);
        let proven = match (&base.kind, &idx.kind) {
            (HKind::Local(l, _), HKind::Local(v, _)) => self.safe_index.contains(&(*l, *v)),
            _ => false,
        };
        if !proven {
            let tl = self.tag_len();
            let len = self.load_t(Ty::I64, b, 0, tl);
            let ok = self.cmp(CmpOp::ULt, i64v, len);
            let pb = self.panic_block("fl_panic_bounds", &[i64v, len]);
            let cont = self.new_block();
            self.branch(ok, cont, pb);
            self.switch(cont);
        }
        let td = self.tag_data();
        let data = self.load_t(Ty::I64, b, 16, td);
        let stride = self.iconst(Ty::I64, Self::stride(et));
        let off = self.bin(BinOp::Mul, i64v, stride);
        self.bin(BinOp::Add, data, off)
    }

    fn widen(&mut self, v: VReg, it: IntTy) -> VReg {
        if it.bits == 64 {
            v
        } else {
            self.cast(if it.signed { CastOp::SExt } else { CastOp::ZExt }, v, Ty::I64)
        }
    }

    // ---------------------------------------------------------- expressions
    /// Evaluate an operand that is only read; owned temporaries are released at statement end.
    fn borrow(&mut self, e: &HExpr) -> VReg {
        let v = self.expr(e);
        if e.ty.is_owned() && !crate::check::is_place(e) && !matches!(e.kind, HKind::Str(_)) {
            if let Some(t) = self.temps.last_mut() {
                t.push((v, e.ty.clone()));
            }
        }
        v
    }

    fn expr(&mut self, e: &HExpr) -> VReg {
        match &e.kind {
            HKind::Int(v) => self.iconst(ir_ty(&e.ty), *v),
            HKind::Float(v) => self.fconst(*v),
            HKind::Bool(b) => self.iconst(Ty::I8, *b as i64),
            HKind::None => self.dummy(),
            HKind::Str(s) => {
                let d = self.lw.string_data(s);
                let dst = self.vreg(Ty::I64);
                self.emit(Inst::DataAddr { dst, data: d });
                dst
            }
            HKind::Local(id, mode) => {
                let r = self.local_regs[*id];
                if *mode == UseMode::Move {
                    // The callee/new owner takes the value; make sure we don't drop it later.
                    let out = self.vreg(ir_ty(&e.ty));
                    self.copy(out, r);
                    if let Some(fr) = self.flags.get(id).copied() {
                        self.emit(Inst::IConst { dst: fr, val: 0 });
                    }
                    out
                } else {
                    r
                }
            }
            HKind::Unary(op, a) => {
                let v = self.borrow(a);
                match op {
                    AU::Neg => {
                        let dst = self.vreg(ir_ty(&e.ty));
                        let o = if e.ty == Type::Float { UnOp::FNeg } else { UnOp::Neg };
                        self.emit(Inst::Un { dst, op: o, a: v });
                        dst
                    }
                    AU::Not => {
                        let one = self.iconst(Ty::I8, 1);
                        self.bin(BinOp::Xor, v, one)
                    }
                    AU::BitNot => {
                        let dst = self.vreg(ir_ty(&e.ty));
                        self.emit(Inst::Un { dst, op: UnOp::Not, a: v });
                        dst
                    }
                }
            }
            HKind::Binary(op, a, b) => self.binary(*op, a, b),
            HKind::Logic(is_and, a, b) => {
                let res = self.vreg(Ty::I8);
                let av = self.cond(a);
                self.copy(res, av);
                let rhs = self.new_block();
                let end = self.new_block();
                if *is_and {
                    self.branch(av, rhs, end);
                } else {
                    self.branch(av, end, rhs);
                }
                self.switch(rhs);
                let bv = self.cond(b);
                self.copy(res, bv);
                self.jump_to(end);
                self.switch(end);
                res
            }
            HKind::Cast(inner) => self.cast_expr(inner, &e.ty),
            HKind::Field(base, idx) => {
                let b = self.borrow(base);
                let Type::Struct(sid) = &base.ty else { unreachable!() };
                let si = &self.lw.structs()[*sid];
                let tf = self.tag_field(*sid, *idx);
                self.load_t(ir_ty(&si.fields[*idx].1), b, si.offsets[*idx] as i32, tf)
            }
            HKind::Index(base, idx) => {
                let addr = self.elem_addr(base, idx);
                let te = self.tag_elem(&e.ty);
                self.load_t(ir_ty(&e.ty), addr, 0, te)
            }
            HKind::Struct(fields) => {
                let Type::Struct(sid) = &e.ty else { unreachable!() };
                let si = &self.lw.structs()[*sid];
                let vals: Vec<VReg> = fields.iter().map(|f| self.expr(f)).collect();
                let sz = self.iconst(Ty::I64, si.size as i64);
                let p = self.rtv("fl_alloc", &[sz]);
                for (i, v) in vals.into_iter().enumerate() {
                    let tf = self.tag_field(*sid, i);
                    self.store_t(p, si.offsets[i] as i32, v, tf);
                }
                p
            }
            HKind::List(elems) => {
                let Type::List(et) = &e.ty else { unreachable!() };
                let vals: Vec<VReg> = elems.iter().map(|x| self.expr(x)).collect();
                let cap = self.iconst(Ty::I64, vals.len() as i64);
                let stride = self.iconst(Ty::I64, Self::stride(et));
                let l = self.rtv("fl_list_new", &[cap, stride]);
                if !vals.is_empty() {
                    let td = self.tag_data();
                    let data = self.load_t(Ty::I64, l, 16, td);
                    let sz = Self::stride(et);
                    let te = self.tag_elem(et);
                    for (i, v) in vals.into_iter().enumerate() {
                        self.store_t(data, (i as i64 * sz) as i32, v, te);
                    }
                    let n = self.iconst(Ty::I64, elems.len() as i64);
                    let tl = self.tag_len();
                    self.store_t(l, 0, n, tl);
                }
                l
            }
            HKind::Call(fid, args) => {
                let callee_locals = &self.lw.prog.funcs[*fid];
                let mut argv = vec![];
                for (i, a) in args.iter().enumerate() {
                    let owns = callee_locals.locals[callee_locals.params[i]].kind.owns();
                    argv.push(if owns { self.expr(a) } else { self.borrow(a) });
                }
                let rty = &callee_locals.ret;
                let dst = if *rty == Type::None { None } else { Some(self.vreg(ir_ty(rty))) };
                self.emit(Inst::Call { dst, callee: Callee::Func(*fid as u32), args: argv });
                match dst {
                    Some(d) => d,
                    None => self.dummy(),
                }
            }
            HKind::Builtin(b, args) => self.builtin(b, args, e),
            HKind::PluginCall { plugin, func, args } => self.plugin_call(*plugin, *func, args, e),
            HKind::ExternCall { index, args } => self.extern_call(*index, args, e),
            HKind::Try(inner) => self.try_expr(inner, &e.ty),
        }
    }

    fn binary(&mut self, op: AB, a: &HExpr, b: &HExpr) -> VReg {
        let av = self.borrow(a);
        let bv = self.borrow(b);
        match &a.ty {
            Type::Int(it) => {
                let it = *it;
                let s = it.signed;
                match op {
                    AB::Add => self.bin(BinOp::Add, av, bv),
                    AB::Sub => self.bin(BinOp::Sub, av, bv),
                    AB::Mul => self.bin(BinOp::Mul, av, bv),
                    AB::Div => self.divide(av, bv, it, false, b),
                    AB::Rem => self.divide(av, bv, it, true, b),
                    AB::BitAnd => self.bin(BinOp::And, av, bv),
                    AB::BitOr => self.bin(BinOp::Or, av, bv),
                    AB::BitXor => self.bin(BinOp::Xor, av, bv),
                    AB::Shl => self.bin(BinOp::Shl, av, bv),
                    AB::Shr => self.bin(if s { BinOp::SShr } else { BinOp::UShr }, av, bv),
                    AB::Eq => self.cmp(CmpOp::Eq, av, bv),
                    AB::Ne => self.cmp(CmpOp::Ne, av, bv),
                    AB::Lt => self.cmp(if s { CmpOp::SLt } else { CmpOp::ULt }, av, bv),
                    AB::Le => self.cmp(if s { CmpOp::SLe } else { CmpOp::ULe }, av, bv),
                    AB::Gt => self.cmp(if s { CmpOp::SGt } else { CmpOp::UGt }, av, bv),
                    AB::Ge => self.cmp(if s { CmpOp::SGe } else { CmpOp::UGe }, av, bv),
                    _ => unreachable!(),
                }
            }
            Type::Float => match op {
                AB::Add => self.bin(BinOp::FAdd, av, bv),
                AB::Sub => self.bin(BinOp::FSub, av, bv),
                AB::Mul => self.bin(BinOp::FMul, av, bv),
                AB::Div => self.bin(BinOp::FDiv, av, bv),
                AB::Eq => self.cmp(CmpOp::FEq, av, bv),
                AB::Ne => self.cmp(CmpOp::FNe, av, bv),
                AB::Lt => self.cmp(CmpOp::FLt, av, bv),
                AB::Le => self.cmp(CmpOp::FLe, av, bv),
                AB::Gt => self.cmp(CmpOp::FGt, av, bv),
                AB::Ge => self.cmp(CmpOp::FGe, av, bv),
                _ => unreachable!(),
            },
            Type::Bool => match op {
                AB::Eq => self.cmp(CmpOp::Eq, av, bv),
                _ => self.cmp(CmpOp::Ne, av, bv),
            },
            Type::Str => match op {
                AB::Add => self.rtv("fl_str_concat", &[av, bv]),
                AB::Eq => self.rtv("fl_str_eq", &[av, bv]),
                AB::Ne => {
                    let r = self.rtv("fl_str_eq", &[av, bv]);
                    let one = self.iconst(Ty::I8, 1);
                    self.bin(BinOp::Xor, r, one)
                }
                _ => {
                    let c = self.rtv("fl_str_cmp", &[av, bv]);
                    let z = self.iconst(Ty::I64, 0);
                    let cop = match op {
                        AB::Lt => CmpOp::SLt,
                        AB::Le => CmpOp::SLe,
                        AB::Gt => CmpOp::SGt,
                        _ => CmpOp::SGe,
                    };
                    self.cmp(cop, c, z)
                }
            },
            t => unreachable!("binary on {:?}", t),
        }
    }

    /// Integer division / remainder with runtime checks (no undefined behaviour).
    fn divide(&mut self, a: VReg, b: VReg, it: IntTy, rem: bool, bexpr: &HExpr) -> VReg {
        let ty = ir_ty(&Type::Int(it));
        let (sop, uop) = if rem { (BinOp::SRem, BinOp::URem) } else { (BinOp::SDiv, BinOp::UDiv) };
        let const_b = match bexpr.kind {
            HKind::Int(v) => Some(v),
            _ => None,
        };
        let safe_const = match const_b {
            Some(0) => false,
            Some(-1) if it.signed => false,
            Some(_) => true,
            None => false,
        };
        if safe_const {
            return self.bin(if it.signed { sop } else { uop }, a, b);
        }
        let zero = self.iconst(ty, 0);
        let is_zero = self.cmp(CmpOp::Eq, b, zero);
        let pb = self.panic_block("fl_panic_divzero", &[]);
        let cont = self.new_block();
        self.branch(is_zero, pb, cont);
        self.switch(cont);
        if !it.signed {
            return self.bin(uop, a, b);
        }
        // x / -1 would overflow for the minimum value: define it as wrapping negation.
        let res = self.vreg(ty);
        let m1 = self.iconst(ty, -1);
        let is_m1 = self.cmp(CmpOp::Eq, b, m1);
        let neg_b = self.new_block();
        let norm_b = self.new_block();
        let join = self.new_block();
        self.branch(is_m1, neg_b, norm_b);
        self.switch(neg_b);
        if rem {
            self.emit(Inst::IConst { dst: res, val: 0 });
        } else {
            self.emit(Inst::Un { dst: res, op: UnOp::Neg, a });
        }
        self.jump_to(join);
        self.switch(norm_b);
        let q = self.bin(sop, a, b);
        self.copy(res, q);
        self.jump_to(join);
        self.switch(join);
        res
    }

    fn cast_expr(&mut self, inner: &HExpr, to: &Type) -> VReg {
        let v = self.borrow(inner);
        match (&inner.ty, to) {
            (Type::Int(f), Type::Int(t)) => self.int_to_int(v, *f, *t),
            (Type::Bool, Type::Int(t)) => {
                let tt = ir_ty(to);
                let _ = t;
                if tt == Ty::I8 {
                    v
                } else {
                    self.cast(CastOp::ZExt, v, tt)
                }
            }
            (Type::Int(f), Type::Float) => {
                let w = self.widen(v, *f);
                self.cast(if f.signed { CastOp::SToF } else { CastOp::UToF }, w, Ty::F64)
            }
            (Type::Float, Type::Float) => v,
            (Type::Float, Type::Int(t)) => {
                let t = *t;
                if t.bits == 64 {
                    return self.cast(if t.signed { CastOp::FToS } else { CastOp::FToU }, v, Ty::I64);
                }
                // Saturate into the target range, then narrow.
                let w = self.cast(CastOp::FToS, v, Ty::I64);
                let lo = self.iconst(Ty::I64, t.min() as i64);
                let hi = self.iconst(Ty::I64, t.max() as i64);
                let lt = self.cmp(CmpOp::SLt, w, lo);
                let c1 = self.vreg(Ty::I64);
                self.emit(Inst::Select { dst: c1, cond: lt, a: lo, b: w });
                let gt = self.cmp(CmpOp::SGt, c1, hi);
                let c2 = self.vreg(Ty::I64);
                self.emit(Inst::Select { dst: c2, cond: gt, a: hi, b: c1 });
                self.cast(CastOp::Trunc, c2, ir_ty(to))
            }
            (a, b) => unreachable!("cast {:?} -> {:?}", a, b),
        }
    }

    fn int_to_int(&mut self, v: VReg, f: IntTy, t: IntTy) -> VReg {
        use std::cmp::Ordering::*;
        let tt = ir_ty(&Type::Int(t));
        match f.bits.cmp(&t.bits) {
            Equal => v,
            Less => self.cast(if f.signed { CastOp::SExt } else { CastOp::ZExt }, v, tt),
            Greater => self.cast(CastOp::Trunc, v, tt),
        }
    }

    // ---------------------------------------------------------------- Result
    fn result_alloc(&mut self, tag: i64) -> VReg {
        let sz = self.iconst(Ty::I64, 24);
        let r = self.rtv("fl_alloc", &[sz]);
        let t = self.iconst(Ty::I64, tag);
        self.store(r, 0, t);
        let z = self.iconst(Ty::I64, 0);
        self.store(r, 8, z);
        self.store(r, 16, z);
        r
    }

    fn try_expr(&mut self, inner: &HExpr, vty: &Type) -> VReg {
        let r = self.expr(inner);
        let tag = self.load(Ty::I64, r, 0);
        let zero = self.iconst(Ty::I64, 0);
        let is_err = self.cmp(CmpOp::Ne, tag, zero);
        let errb = self.new_block();
        let okb = self.new_block();
        self.branch(is_err, errb, okb);
        self.switch(errb);
        if self.is_main {
            let msg = self.load(Ty::I64, r, 16);
            self.rt("fl_fatal_error", &[msg]);
            self.term(Term::Unreachable);
        } else {
            // Move the message into a fresh error Result of the function's return type.
            let nr = self.result_alloc(1);
            let msg = self.load(Ty::I64, r, 16);
            self.store(nr, 16, msg);
            let sz = self.iconst(Ty::I64, 24);
            self.rt("fl_free", &[r, sz]);
            self.pending_temps_drop();
            self.drop_scopes_to(0);
            self.term(Term::Ret(Some(nr)));
        }
        self.switch(okb);
        let v = if *vty == Type::None { self.dummy() } else { self.load(ir_ty(vty), r, 8) };
        let sz = self.iconst(Ty::I64, 24);
        self.rt("fl_free", &[r, sz]);
        v
    }

    // --------------------------------------------------------------- extern fn
    /// Call a C-ABI function: integers are widened to 64 bits (sign- or zero-extended per their
    /// Flash type, which satisfies every C ABI), `Bool` is 0/1, `String` becomes (pointer, length).
    fn extern_call(&mut self, index: usize, args: &[HExpr], e: &HExpr) -> VReg {
        let ef = &self.lw.prog.externs[index];
        let mut cparams: Vec<Ty> = vec![];
        let mut argv: Vec<VReg> = vec![];
        for a in args {
            let v = self.borrow(a);
            match &a.ty {
                Type::Int(it) => {
                    argv.push(self.widen(v, *it));
                    cparams.push(Ty::I64);
                }
                Type::Float => {
                    argv.push(v);
                    cparams.push(Ty::F64);
                }
                Type::Bool => {
                    argv.push(self.cast(CastOp::ZExt, v, Ty::I64));
                    cparams.push(Ty::I64);
                }
                Type::Str => {
                    let sixteen = self.iconst(Ty::I64, 16);
                    argv.push(self.bin(BinOp::Add, v, sixteen));
                    let tl = self.tag_len();
                    argv.push(self.load_t(Ty::I64, v, 0, tl));
                    cparams.push(Ty::I64);
                    cparams.push(Ty::I64);
                }
                t => unreachable!("extern parameter {:?}", t),
            }
        }
        let ret = if ef.ret == Type::None { None } else { Some(ir_ty(&ef.ret)) };
        let name = ef.name.clone();
        let ext = self.lw.m.extern_ref(&name, &cparams, ret);
        let dst = ret.map(|t| self.vreg(t));
        self.emit(Inst::Call { dst, callee: Callee::Extern(ext), args: argv });
        match dst {
            Some(d) if e.ty == Type::Bool => {
                // normalise any non-zero C bool to exactly 1
                let z = self.iconst(Ty::I8, 0);
                self.cmp(CmpOp::Ne, d, z)
            }
            Some(d) => d,
            None => self.dummy(),
        }
    }

    // --------------------------------------------------------------- plugins
    fn emit_plugin_loads(&mut self) {
        for i in 0..self.lw.prog.plugins.len() {
            let desc = crate::plugins::descriptor(&self.lw.prog.plugins[i]);
            let d = self.lw.raw_data("plugin", desc.clone().into_bytes());
            let p = self.vreg(Ty::I64);
            self.emit(Inst::DataAddr { dst: p, data: d });
            let n = self.iconst(Ty::I64, desc.len() as i64);
            self.rt("fl_plugin_load", &[p, n]);
        }
    }

    fn plugin_call(&mut self, plugin: usize, func: usize, args: &[HExpr], e: &HExpr) -> VReg {
        let pf = &self.lw.prog.plugins[plugin].funcs[func];
        let fallible = pf.fallible;
        let n = args.len().max(1) as u32;
        // FlashValue is 24 bytes: u32 tag, u32 pad, u64 a, u64 b.
        let slot_args = self.f.stack_slots.len() as u32;
        self.f.stack_slots.push(24 * n);
        let slot_ret = self.f.stack_slots.len() as u32;
        self.f.stack_slots.push(24);
        let abase = self.vreg(Ty::I64);
        self.emit(Inst::StackAddr { dst: abase, slot: slot_args });
        let rbase = self.vreg(Ty::I64);
        self.emit(Inst::StackAddr { dst: rbase, slot: slot_ret });
        for (i, a) in args.iter().enumerate() {
            let v = self.borrow(a);
            let off = (i * 24) as i32;
            let (tag, payload): (i64, Option<VReg>) = match &a.ty {
                Type::Int(it) => {
                    let w = self.widen(v, *it);
                    (if it.signed { 2 } else { 3 }, Some(w))
                }
                Type::Float => (4, Some(v)),
                Type::Bool => {
                    let w = self.cast(CastOp::ZExt, v, Ty::I64);
                    (1, Some(w))
                }
                Type::Str => {
                    let len = self.load(Ty::I64, v, 0);
                    let sixteen = self.iconst(Ty::I64, 16);
                    let ptr = self.bin(BinOp::Add, v, sixteen);
                    self.store(abase, off + 8, ptr);
                    self.store(abase, off + 16, len);
                    (5, None)
                }
                _ => (0, None),
            };
            let t = self.iconst(Ty::I32, tag);
            self.store(abase, off, t);
            if let Some(p) = payload {
                self.store(abase, off + 8, p);
            }
        }
        let pi = self.iconst(Ty::I64, plugin as i64);
        let fi = self.iconst(Ty::I64, func as i64);
        let status = self.rtv("fl_plugin_call", &[pi, fi, abase, rbase]);
        let inner_ty = if fallible {
            match &e.ty {
                Type::Result(t) => (**t).clone(),
                _ => unreachable!(),
            }
        } else {
            e.ty.clone()
        };
        let zero = self.iconst(Ty::I64, 0);
        let failed = self.cmp(CmpOp::Ne, status, zero);
        let result = self.vreg(ir_ty(&e.ty));
        let errb = self.new_block();
        let okb = self.new_block();
        let join = self.new_block();
        self.branch(failed, errb, okb);
        // failure: ret slot holds a Flash String with the message
        self.switch(errb);
        let msg = self.load(Ty::I64, rbase, 8);
        if fallible {
            let nr = self.result_alloc(1);
            self.store(nr, 16, msg);
            self.copy(result, nr);
            self.jump_to(join);
        } else {
            self.rt("fl_fatal_error", &[msg]);
            self.term(Term::Unreachable);
        }
        self.switch(okb);
        let val = match &inner_ty {
            Type::None => self.dummy(),
            Type::Float => self.load(Ty::F64, rbase, 8),
            Type::Int(it) => {
                let raw = self.load(Ty::I64, rbase, 8);
                self.int_to_int(raw, crate::types::INT, *it)
            }
            Type::Bool => {
                let raw = self.load(Ty::I64, rbase, 8);
                self.cast(CastOp::Trunc, raw, Ty::I8)
            }
            Type::Str => self.load(Ty::I64, rbase, 8),
            t => unreachable!("plugin return type {:?}", t),
        };
        if fallible {
            let nr = self.result_alloc(0);
            if inner_ty != Type::None {
                self.store(nr, 8, val);
            }
            self.copy(result, nr);
        } else {
            self.copy(result, val);
        }
        self.jump_to(join);
        self.switch(join);
        result
    }
}

#[path = "lower_builtins.rs"]
mod lower_builtins;
