//! Abstract syntax tree produced by the parser.

use crate::source::Span;

#[derive(Clone, Debug)]
pub struct Module {
    pub file: u32,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug)]
pub enum Item {
    Fn(FnDef),
    Struct(StructDef),
    /// `extern fn name(a: Int) -> Int` — a C-ABI function provided by a linked library.
    ExternFn(ExternFnDef),
    /// `import name` — another Flash source file.
    Import(ImportDecl),
    /// `use rust_plugin "name"` / `use cpp_plugin "name"`.
    UsePlugin(PluginDecl),
    Stmt(Stmt),
}

#[derive(Clone, Debug)]
pub struct ImportDecl {
    pub path: Vec<String>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginKind {
    Rust,
    Cpp,
}

#[derive(Clone, Debug)]
pub struct PluginDecl {
    pub kind: PluginKind,
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamMode {
    /// Read-only borrow (default).
    Borrow,
    /// Exclusive mutable borrow.
    Mut,
    /// Takes ownership.
    Own,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: Option<TypeExpr>,
    pub mode: ParamMode,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FnDef {
    pub name: String,
    pub name_span: Span,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ExternFnDef {
    pub name: String,
    pub name_span: Span,
    pub params: Vec<(String, TypeExpr)>,
    pub ret: Option<TypeExpr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FieldDef {
    pub name: String,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct StructDef {
    pub name: String,
    pub name_span: Span,
    pub fields: Vec<FieldDef>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct TypeExpr {
    pub name: String,
    pub args: Vec<TypeExpr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    /// `target = value` or `target: Type = value`
    Assign {
        target: Expr,
        ty: Option<TypeExpr>,
        value: Expr,
    },
    AugAssign {
        target: Expr,
        op: BinOp,
        value: Expr,
    },
    Expr(Expr),
    If {
        branches: Vec<(Expr, Vec<Stmt>)>,
        else_body: Option<Vec<Stmt>>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    For {
        var: String,
        var_span: Span,
        iter: ForIter,
        body: Vec<Stmt>,
    },
    Break,
    Continue,
    Return(Option<Expr>),
    Pass,
}

#[derive(Clone, Debug)]
pub enum ForIter {
    /// `lo..hi` (exclusive) or `lo..=hi` (inclusive)
    Range {
        lo: Expr,
        hi: Expr,
        inclusive: bool,
    },
    Expr(Expr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "and",
            BinOp::Or => "or",
        }
    }
    pub fn is_comparison(self) -> bool {
        matches!(self, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Clone, Debug)]
pub struct Arg {
    pub name: Option<String>,
    pub value: Expr,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int(i128),
    Float(f64),
    Str(String),
    Bool(bool),
    None,
    Name(String),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },
    Field {
        base: Box<Expr>,
        name: String,
        name_span: Span,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    List(Vec<Expr>),
    /// `expr?` — propagate an error.
    Try(Box<Expr>),
}
