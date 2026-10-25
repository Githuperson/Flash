//! The Flash compiler library. See ARCHITECTURE.md for an overview of the stages:
//! source → lexer → parser → AST → resolver → checker (HIR) → borrow checker → lowering (IR)
//! → optimiser → Cranelift codegen → linker.

#![allow(
    clippy::result_unit_err,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::needless_range_loop,
    clippy::if_same_then_else,
    clippy::option_map_unit_fn
)]

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
#[cfg(feature = "legacy-backends")]
pub mod llvm;
pub mod lower;
pub mod module;
pub mod native;
pub mod opt;
pub mod parser;
pub mod pkg;
pub mod plugins;
pub mod resolve;
pub mod rtsigs;
pub mod source;
pub mod types;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
