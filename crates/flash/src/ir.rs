//! Backend-independent low-level IR.
//!
//! A function is a control-flow graph of basic blocks over *mutable* virtual registers
//! (not SSA); the Cranelift backend builds SSA from them. All heap values are `I64` pointers.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    I8,
    I16,
    I32,
    I64,
    F64,
}

impl Ty {
    pub fn bytes(self) -> u32 {
        match self {
            Ty::I8 => 1,
            Ty::I16 => 2,
            Ty::I32 => 4,
            Ty::I64 | Ty::F64 => 8,
        }
    }
    pub fn is_float(self) -> bool {
        self == Ty::F64
    }
}

pub type VReg = u32;
pub type BlockId = u32;
pub type FuncRef = u32;
pub type ExternRef = u32;
pub type DataId = u32;
/// Alias-analysis class of a memory access (0 = unknown: may alias anything). Back ends that
/// understand type-based alias analysis (the LLVM back end) use it; others ignore it.
pub type MemTag = u16;
pub const NO_TAG: MemTag = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    SDiv,
    UDiv,
    SRem,
    URem,
    And,
    Or,
    Xor,
    Shl,
    SShr,
    UShr,
    FAdd,
    FSub,
    FMul,
    FDiv,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CmpOp {
    Eq,
    Ne,
    SLt,
    SLe,
    SGt,
    SGe,
    ULt,
    ULe,
    UGt,
    UGe,
    FEq,
    FNe,
    FLt,
    FLe,
    FGt,
    FGe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnOp {
    Neg,
    Not,
    FNeg,
    FSqrt,
    FFloor,
    FCeil,
    FAbs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CastOp {
    /// Zero-extend an integer to a wider type.
    ZExt,
    SExt,
    /// Truncate to a narrower integer.
    Trunc,
    SToF,
    UToF,
    /// Saturating float → signed int.
    FToS,
    FToU,
    /// Reinterpret float bits as I64 and back.
    FBitsToI,
    IBitsToF,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Callee {
    Func(FuncRef),
    Extern(ExternRef),
    Indirect(VReg),
    /// A Linux system call (native back end only); `args[0]` is the call number, followed by up
    /// to six arguments. Used by the runtime library written in Flash.
    Syscall,
}

#[derive(Clone, Debug)]
pub enum Inst {
    IConst {
        dst: VReg,
        val: i64,
    },
    FConst {
        dst: VReg,
        val: f64,
    },
    Copy {
        dst: VReg,
        src: VReg,
    },
    Bin {
        dst: VReg,
        op: BinOp,
        a: VReg,
        b: VReg,
    },
    Cmp {
        dst: VReg,
        op: CmpOp,
        a: VReg,
        b: VReg,
    },
    Un {
        dst: VReg,
        op: UnOp,
        a: VReg,
    },
    Cast {
        dst: VReg,
        op: CastOp,
        src: VReg,
    },
    Load {
        dst: VReg,
        base: VReg,
        off: i32,
        tag: MemTag,
    },
    Store {
        base: VReg,
        off: i32,
        val: VReg,
        tag: MemTag,
    },
    Call {
        dst: Option<VReg>,
        callee: Callee,
        args: Vec<VReg>,
    },
    /// `dst = *(base + (index << scale) + off)`. Produced only by the native back end's last
    /// pass (address folding); the machine-independent optimiser never sees it.
    LoadIdx {
        dst: VReg,
        base: VReg,
        index: VReg,
        scale: u8,
        off: i32,
        tag: MemTag,
    },
    /// `*(base + (index << scale) + off) = val` (see `LoadIdx`).
    StoreIdx {
        base: VReg,
        index: VReg,
        scale: u8,
        off: i32,
        val: VReg,
        tag: MemTag,
    },
    /// Address of read-only/static data.
    DataAddr {
        dst: VReg,
        data: DataId,
    },
    /// The calling thread's runtime state block (Linux only): the thread pointer register
    /// (`fs` base on x86-64, `tpidr_el0` on AArch64) holds it.
    ThreadPtr {
        dst: VReg,
    },
    /// Address of a function-local stack slot.
    StackAddr {
        dst: VReg,
        slot: u32,
    },
    /// dst = cond ? a : b
    Select {
        dst: VReg,
        cond: VReg,
        a: VReg,
        b: VReg,
    },
}

#[derive(Clone, Debug)]
pub enum Term {
    Jump(BlockId),
    Br { cond: VReg, then_: BlockId, else_: BlockId },
    Ret(Option<VReg>),
    Unreachable,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub insts: Vec<Inst>,
    pub term: Term,
}

#[derive(Clone, Debug)]
pub struct Function {
    pub name: String,
    /// Parameter vregs are `0..params.len()`.
    pub params: Vec<Ty>,
    pub ret: Option<Ty>,
    pub vregs: Vec<Ty>,
    pub blocks: Vec<Block>,
    pub stack_slots: Vec<u32>,
    pub exported: bool,
    /// Never inline (e.g. recursive or large).
    pub no_inline: bool,
}

#[derive(Clone, Debug)]
pub struct ExternDecl {
    pub name: String,
    pub params: Vec<Ty>,
    pub ret: Option<Ty>,
}

#[derive(Clone, Debug)]
pub struct DataItem {
    pub name: String,
    pub bytes: Vec<u8>,
    pub align: u32,
    pub writable: bool,
}

#[derive(Debug, Default)]
pub struct Module {
    pub funcs: Vec<Function>,
    pub externs: Vec<ExternDecl>,
    pub data: Vec<DataItem>,
    /// Names of memory alias classes; `tags[0]` is unused (`NO_TAG`).
    pub tags: Vec<String>,
}

impl Module {
    pub fn extern_ref(&mut self, name: &str, params: &[Ty], ret: Option<Ty>) -> ExternRef {
        if let Some(i) = self.externs.iter().position(|e| e.name == name) {
            return i as u32;
        }
        self.externs.push(ExternDecl { name: name.to_string(), params: params.to_vec(), ret });
        (self.externs.len() - 1) as u32
    }
}

impl Function {
    pub fn new_vreg(&mut self, ty: Ty) -> VReg {
        self.vregs.push(ty);
        (self.vregs.len() - 1) as u32
    }
}

impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for func in &self.funcs {
            writeln!(f, "fn {}({:?}) -> {:?} {{", func.name, func.params, func.ret)?;
            for (i, b) in func.blocks.iter().enumerate() {
                writeln!(f, "  b{}:", i)?;
                for inst in &b.insts {
                    writeln!(f, "    {:?}", inst)?;
                }
                writeln!(f, "    {:?}", b.term)?;
            }
            writeln!(f, "}}")?;
        }
        Ok(())
    }
}
