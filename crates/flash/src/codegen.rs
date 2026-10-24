//! Cranelift backend: IR → native object file.
//!
//! This is the only module that knows about Cranelift. The frontend, IR and optimiser are
//! backend-independent so other code generators (e.g. LLVM for release builds) can be added.

use std::collections::HashMap;

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{
    self, types, AbiParam, InstBuilder, MemFlagsData, StackSlotData, StackSlotKind, TrapCode, UserFuncName,
};
use cranelift_codegen::isa::OwnedTargetIsa;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{default_libcall_names, DataDescription, FuncId, Linkage, Module as _};
use cranelift_object::{ObjectBuilder, ObjectModule};

use crate::ir::{self as fir, BinOp, Callee, CastOp, CmpOp, Inst, Term, Ty, UnOp};

pub struct CodegenOptions {
    pub release: bool,
    /// Target triple; `None` means the host.
    pub target: Option<String>,
}

fn clif_ty(t: Ty) -> ir::Type {
    match t {
        Ty::I8 => types::I8,
        Ty::I16 => types::I16,
        Ty::I32 => types::I32,
        Ty::I64 => types::I64,
        Ty::F64 => types::F64,
    }
}

pub fn make_isa(opts: &CodegenOptions) -> Result<OwnedTargetIsa, String> {
    let mut flags = settings::builder();
    flags.set("opt_level", if opts.release { "speed" } else { "none" }).map_err(|e| e.to_string())?;
    flags.set("is_pic", "true").map_err(|e| e.to_string())?;
    flags.set("enable_verifier", if opts.release { "false" } else { "true" }).map_err(|e| e.to_string())?;
    let flags = settings::Flags::new(flags);
    match &opts.target {
        None => {
            let b = if opts.release {
                cranelift_native::builder()?
            } else {
                cranelift_native::builder_with_options(false)?
            };
            b.finish(flags).map_err(|e| e.to_string())
        }
        Some(t) => {
            let triple: target_lexicon::Triple = t.parse().map_err(|e| format!("invalid target triple '{}': {}", t, e))?;
            let b = cranelift_codegen::isa::lookup(triple).map_err(|e| format!("unsupported target '{}': {}", t, e))?;
            b.finish(flags).map_err(|e| e.to_string())
        }
    }
}

pub fn compile(m: &fir::Module, opts: &CodegenOptions) -> Result<Vec<u8>, String> {
    let isa = make_isa(opts)?;
    let call_conv = isa.default_call_conv();
    let builder = ObjectBuilder::new(isa, "flash_program", default_libcall_names()).map_err(|e| e.to_string())?;
    let mut module = ObjectModule::new(builder);

    let sig_of = |params: &[Ty], ret: Option<Ty>| {
        let mut s = ir::Signature::new(call_conv);
        for p in params {
            s.params.push(AbiParam::new(clif_ty(*p)));
        }
        if let Some(r) = ret {
            s.returns.push(AbiParam::new(clif_ty(r)));
        }
        s
    };

    // Runtime imports.
    let mut ext_ids: Vec<FuncId> = vec![];
    for e in &m.externs {
        let id = module
            .declare_function(&e.name, Linkage::Import, &sig_of(&e.params, e.ret))
            .map_err(|er| er.to_string())?;
        ext_ids.push(id);
    }
    // Functions.
    let mut func_ids: Vec<FuncId> = vec![];
    for f in &m.funcs {
        let link = if f.exported { Linkage::Export } else { Linkage::Local };
        let id = module.declare_function(&f.name, link, &sig_of(&f.params, f.ret)).map_err(|er| er.to_string())?;
        func_ids.push(id);
    }
    // Data.
    let mut data_ids = vec![];
    for d in &m.data {
        let id = module.declare_data(&d.name, Linkage::Local, d.writable, false).map_err(|e| e.to_string())?;
        let mut desc = DataDescription::new();
        desc.define(d.bytes.clone().into_boxed_slice());
        desc.set_align(d.align as u64);
        module.define_data(id, &desc).map_err(|e| e.to_string())?;
        data_ids.push(id);
    }

    let fe_cfg = module.isa().frontend_config();
    let mut fctx = FunctionBuilderContext::new();
    for (fi, f) in m.funcs.iter().enumerate() {
        let mut ctx = module.make_context();
        ctx.func.signature = sig_of(&f.params, f.ret);
        ctx.func.name = UserFuncName::user(0, fi as u32);
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
            // Declare SSA variables for every virtual register.
            let vars: Vec<Variable> = f.vregs.iter().map(|t| b.declare_var(clif_ty(*t))).collect();
            let blocks: Vec<ir::Block> = f.blocks.iter().map(|_| b.create_block()).collect();
            let slots: Vec<ir::StackSlot> = f
                .stack_slots
                .iter()
                .map(|sz| b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, *sz, 3)))
                .collect();
            // Local FuncRefs.
            let mut fref_cache: HashMap<FuncId, ir::FuncRef> = HashMap::new();
            let entry = blocks[0];
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            for i in 0..f.params.len() {
                let v = b.block_params(entry)[i];
                b.def_var(vars[i], v);
            }
            for (bi, blk) in f.blocks.iter().enumerate() {
                if bi != 0 {
                    b.switch_to_block(blocks[bi]);
                }
                for inst in &blk.insts {
                    emit_inst(&mut b, &mut module, inst, &vars, &slots, &ext_ids, &func_ids, &data_ids, &mut fref_cache, f);
                }
                match &blk.term {
                    Term::Jump(t) => {
                        b.ins().jump(blocks[*t as usize], &[]);
                    }
                    Term::Br { cond, then_, else_ } => {
                        let c = b.use_var(vars[*cond as usize]);
                        b.ins().brif(c, blocks[*then_ as usize], &[], blocks[*else_ as usize], &[]);
                    }
                    Term::Ret(v) => match v {
                        Some(v) if f.ret.is_some() => {
                            let val = b.use_var(vars[*v as usize]);
                            b.ins().return_(&[val]);
                        }
                        _ => {
                            b.ins().return_(&[]);
                        }
                    },
                    Term::Unreachable => {
                        b.ins().trap(TrapCode::unwrap_user(1));
                    }
                }
            }
            b.seal_all_blocks();
            b.finalize(fe_cfg);
        }
        module.define_function(func_ids[fi], &mut ctx).map_err(|e| format!("codegen of '{}' failed: {}", f.name, e))?;
        module.clear_context(&mut ctx);
    }
    let product = module.finish();
    product.emit().map_err(|e| e.to_string())
}

#[allow(clippy::too_many_arguments)]
fn emit_inst(
    b: &mut FunctionBuilder,
    module: &mut ObjectModule,
    inst: &Inst,
    vars: &[Variable],
    slots: &[ir::StackSlot],
    ext_ids: &[FuncId],
    func_ids: &[FuncId],
    data_ids: &[cranelift_module::DataId],
    frefs: &mut HashMap<FuncId, ir::FuncRef>,
    f: &fir::Function,
) {
    macro_rules! u {
        ($v:expr) => {
            b.use_var(vars[*$v as usize])
        };
    }
    macro_rules! d {
        ($dst:expr, $val:expr) => {{
            let v = $val;
            b.def_var(vars[*$dst as usize], v);
        }};
    }
    let mf = MemFlagsData::trusted();
    match inst {
        Inst::IConst { dst, val } => {
            let ty = clif_ty(f.vregs[*dst as usize]);
            d!(dst, b.ins().iconst(ty, *val));
        }
        Inst::FConst { dst, val } => d!(dst, b.ins().f64const(*val)),
        Inst::Copy { dst, src } => {
            let v = u!(src);
            d!(dst, v);
        }
        Inst::Bin { dst, op, a, b: bb } => {
            let (x, y) = (u!(a), u!(bb));
            let r = match op {
                BinOp::Add => b.ins().iadd(x, y),
                BinOp::Sub => b.ins().isub(x, y),
                BinOp::Mul => b.ins().imul(x, y),
                BinOp::SDiv => b.ins().sdiv(x, y),
                BinOp::UDiv => b.ins().udiv(x, y),
                BinOp::SRem => b.ins().srem(x, y),
                BinOp::URem => b.ins().urem(x, y),
                BinOp::And => b.ins().band(x, y),
                BinOp::Or => b.ins().bor(x, y),
                BinOp::Xor => b.ins().bxor(x, y),
                BinOp::Shl => b.ins().ishl(x, y),
                BinOp::SShr => b.ins().sshr(x, y),
                BinOp::UShr => b.ins().ushr(x, y),
                BinOp::FAdd => b.ins().fadd(x, y),
                BinOp::FSub => b.ins().fsub(x, y),
                BinOp::FMul => b.ins().fmul(x, y),
                BinOp::FDiv => b.ins().fdiv(x, y),
            };
            d!(dst, r);
        }
        Inst::Cmp { dst, op, a, b: bb } => {
            let (x, y) = (u!(a), u!(bb));
            let r = match op {
                CmpOp::Eq => b.ins().icmp(IntCC::Equal, x, y),
                CmpOp::Ne => b.ins().icmp(IntCC::NotEqual, x, y),
                CmpOp::SLt => b.ins().icmp(IntCC::SignedLessThan, x, y),
                CmpOp::SLe => b.ins().icmp(IntCC::SignedLessThanOrEqual, x, y),
                CmpOp::SGt => b.ins().icmp(IntCC::SignedGreaterThan, x, y),
                CmpOp::SGe => b.ins().icmp(IntCC::SignedGreaterThanOrEqual, x, y),
                CmpOp::ULt => b.ins().icmp(IntCC::UnsignedLessThan, x, y),
                CmpOp::ULe => b.ins().icmp(IntCC::UnsignedLessThanOrEqual, x, y),
                CmpOp::UGt => b.ins().icmp(IntCC::UnsignedGreaterThan, x, y),
                CmpOp::UGe => b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, x, y),
                CmpOp::FEq => b.ins().fcmp(FloatCC::Equal, x, y),
                CmpOp::FNe => b.ins().fcmp(FloatCC::NotEqual, x, y),
                CmpOp::FLt => b.ins().fcmp(FloatCC::LessThan, x, y),
                CmpOp::FLe => b.ins().fcmp(FloatCC::LessThanOrEqual, x, y),
                CmpOp::FGt => b.ins().fcmp(FloatCC::GreaterThan, x, y),
                CmpOp::FGe => b.ins().fcmp(FloatCC::GreaterThanOrEqual, x, y),
            };
            d!(dst, r);
        }
        Inst::Un { dst, op, a } => {
            let x = u!(a);
            let r = match op {
                UnOp::Neg => b.ins().ineg(x),
                UnOp::Not => b.ins().bnot(x),
                UnOp::FNeg => b.ins().fneg(x),
            };
            d!(dst, r);
        }
        Inst::Cast { dst, op, src } => {
            let x = u!(src);
            let to = clif_ty(f.vregs[*dst as usize]);
            let r = match op {
                CastOp::ZExt => b.ins().uextend(to, x),
                CastOp::SExt => b.ins().sextend(to, x),
                CastOp::Trunc => b.ins().ireduce(to, x),
                CastOp::SToF => b.ins().fcvt_from_sint(to, x),
                CastOp::UToF => b.ins().fcvt_from_uint(to, x),
                CastOp::FToS => b.ins().fcvt_to_sint_sat(to, x),
                CastOp::FToU => b.ins().fcvt_to_uint_sat(to, x),
                CastOp::FBitsToI => b.ins().bitcast(to, MemFlagsData::new(), x),
                CastOp::IBitsToF => b.ins().bitcast(to, MemFlagsData::new(), x),
            };
            d!(dst, r);
        }
        Inst::Load { dst, base, off } => {
            let ty = clif_ty(f.vregs[*dst as usize]);
            let p = u!(base);
            d!(dst, b.ins().load(ty, mf, p, *off));
        }
        Inst::Store { base, off, val } => {
            let p = u!(base);
            let v = u!(val);
            b.ins().store(mf, v, p, *off);
        }
        Inst::Call { dst, callee, args } => {
            let id = match callee {
                Callee::Func(i) => func_ids[*i as usize],
                Callee::Extern(i) => ext_ids[*i as usize],
                Callee::Indirect(_) => unreachable!("indirect calls are not generated yet"),
            };
            let fref = *frefs.entry(id).or_insert_with(|| module.declare_func_in_func(id, b.func));
            let argv: Vec<ir::Value> = args.iter().map(|a| u!(a)).collect();
            let call = b.ins().call(fref, &argv);
            if let Some(dst) = dst {
                let r = b.inst_results(call)[0];
                d!(dst, r);
            }
        }
        Inst::DataAddr { dst, data } => {
            let gv = module.declare_data_in_func(data_ids[*data as usize], b.func);
            d!(dst, b.ins().symbol_value(types::I64, gv));
        }
        Inst::StackAddr { dst, slot } => {
            d!(dst, b.ins().stack_addr(types::I64, slots[*slot as usize], 0));
        }
        Inst::Select { dst, cond, a, b: bb } => {
            let (c, x, y) = (u!(cond), u!(a), u!(bb));
            d!(dst, b.ins().select(c, x, y));
        }
    }
}
