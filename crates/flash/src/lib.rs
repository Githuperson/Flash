//! The Flash compiler library. See ARCHITECTURE.md for an overview of the stages:
//! source → lexer → parser → AST → resolver → checker (HIR) → borrow checker → lowering (IR)
//! → optimiser → Cranelift codegen → linker.

pub mod ast;
pub mod borrowck;
pub mod builtins;
pub mod check;
mod check_builtins;
pub mod codegen;
pub mod diag;
pub mod driver;
pub mod fmt;
pub mod hir;
pub mod ir;
pub mod lexer;
pub mod lower;
pub mod module;
pub mod opt;
pub mod parser;
pub mod pkg;
pub mod plugins;
pub mod resolve;
mod rtsigs;
pub mod source;
pub mod types;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
