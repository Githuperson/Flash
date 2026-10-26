//! Typed, resolved intermediate tree produced by the checker ("HIR").
//!
//! Every function here is a *monomorphic instance*: parameter and return types are concrete.
//! Ownership decisions (copy / move / borrow) are already explicit.

use crate::ast::{BinOp, UnOp};
use crate::source::Span;
use crate::types::{EnumInfo, IntTy, StructInfo, Type};

pub type LocalId = usize;
pub type FuncId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UseMode {
    /// Plain copy of a Copy-type value.
    Copy,
    /// Ownership moves out of the variable.
    Move,
    /// Temporarily read, ownership stays.
    Borrow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalKind {
    /// Ordinary variable that owns its value.
    Var,
    /// Read-only borrowed parameter.
    ParamBorrow,
    /// Exclusively borrowed (mutable) parameter.
    ParamMut,
    /// Owned parameter.
    ParamOwn,
    /// Loop variable that borrows an element of the iterated list.
    LoopRef,
}

impl LocalKind {
    pub fn owns(self) -> bool {
        matches!(self, LocalKind::Var | LocalKind::ParamOwn)
    }
}

#[derive(Clone, Debug)]
pub struct LocalInfo {
    pub name: String,
    pub ty: Type,
    pub kind: LocalKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Builtin {
    Say,
    Print,
    StrOf,
    ToInt(IntTy),
    ToFloat,
    Len,
    Append,
    Pop,
    Copy,
    Fill,
    Abs,
    Min,
    Max,
    MathF(&'static str),
    Pow,
    Exit,
    Assert,
    NowNs,
    Args,
    ReadLine,
    Upper,
    Lower,
    Trim,
    Contains,
    StartsWith,
    EndsWith,
    Replace,
    Split,
    Join,
    Find,
    Substr,
    ByteAt,
    ParseInt,
    ParseFloat,
    ReadFile,
    WriteFile,
    AppendFile,
    FileExists,
    RemoveFile,
    EnvVar,
    PathJoin,
    PathFilename,
    PathParent,
    PathExt,
    SleepMs,
    Ok,
    Fail,
    IsOk,
    Unwrap,
    UnwrapOr,
    ErrorMsg,
    /// Low-level operation of the runtime library, by name (`__peek`, `__syscall`, …).
    Intrinsic(&'static str),
}

#[derive(Clone, Debug)]
pub struct HExpr {
    pub kind: HKind,
    pub ty: Type,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum HKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    None,
    Local(LocalId, UseMode),
    Unary(UnOp, Box<HExpr>),
    /// Arithmetic / comparison. Both operands have the same type.
    Binary(BinOp, Box<HExpr>, Box<HExpr>),
    /// Short-circuit `and` (true) / `or` (false).
    Logic(bool, Box<HExpr>, Box<HExpr>),
    Call(FuncId, Vec<HExpr>),
    Builtin(Builtin, Vec<HExpr>),
    PluginCall {
        plugin: usize,
        func: usize,
        args: Vec<HExpr>,
    },
    /// Call of a C-ABI function declared with `extern fn`.
    ExternCall {
        index: usize,
        args: Vec<HExpr>,
    },
    Struct(Vec<HExpr>),
    /// A new enum value: variant `variant` of enum `enum_id` with its payload.
    Variant {
        enum_id: usize,
        variant: usize,
        fields: Vec<HExpr>,
    },
    /// The variant number of an enum value (as Int).
    EnumTag(Box<HExpr>),
    /// Payload field `.2` of variant `.1` of an enum value (only valid when the tag matches).
    EnumField(Box<HExpr>, usize, usize),
    Field(Box<HExpr>, usize),
    Index(Box<HExpr>, Box<HExpr>),
    List(Vec<HExpr>),
    /// Numeric conversion of the inner expression to `self.ty`.
    Cast(Box<HExpr>),
    /// `expr?` — unwrap Result or return the error.
    Try(Box<HExpr>),
}

#[derive(Clone, Debug)]
pub struct HStmt {
    pub kind: HStmtKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum HStmtKind {
    /// Declare a new variable and initialise it.
    Let(LocalId, HExpr),
    /// Assign to an existing place (Local, Field or Index expression).
    Assign(HExpr, HExpr),
    Expr(HExpr),
    If(HExpr, Vec<HStmt>, Vec<HStmt>),
    While(HExpr, Vec<HStmt>),
    ForRange {
        var: LocalId,
        lo: HExpr,
        hi: HExpr,
        inclusive: bool,
        body: Vec<HStmt>,
    },
    ForList {
        var: LocalId,
        list: HExpr,
        body: Vec<HStmt>,
    },
    /// Run `body` while `root` is borrowed (it cannot be modified or moved): the arms of a `match`
    /// whose bindings point into the matched value.
    Borrowed {
        root: LocalId,
        body: Vec<HStmt>,
    },
    Break,
    Continue,
    Return(Option<HExpr>),
}

#[derive(Clone, Debug)]
pub struct HFunc {
    pub name: String,
    pub params: Vec<LocalId>,
    pub locals: Vec<LocalInfo>,
    pub ret: Type,
    pub body: Vec<HStmt>,
    pub span: Span,
    pub is_main: bool,
}

#[derive(Clone, Debug)]
pub struct PluginFunc {
    pub name: String,
    pub params: Vec<Type>,
    pub ret: Type,
    /// The native function may report an error; Flash sees `Result[ret]`.
    pub fallible: bool,
}

#[derive(Clone, Debug)]
pub struct PluginBinding {
    /// Name used in Flash source (`fast_math`).
    pub name: String,
    pub kind: crate::ast::PluginKind,
    /// Absolute path of the manifest this binding was read from.
    pub manifest_dir: std::path::PathBuf,
    pub library: String,
    pub abi_version: u32,
    pub integrity: Option<String>,
    pub funcs: Vec<PluginFunc>,
}

#[derive(Clone, Debug)]
pub struct ExternFn {
    pub name: String,
    pub params: Vec<(String, Type)>,
    pub ret: Type,
}

#[derive(Debug)]
pub struct Program {
    pub funcs: Vec<HFunc>,
    pub structs: Vec<StructInfo>,
    pub enums: Vec<EnumInfo>,
    pub main: FuncId,
    pub plugins: Vec<PluginBinding>,
    pub externs: Vec<ExternFn>,
}
