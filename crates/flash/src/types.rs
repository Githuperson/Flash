//! The Flash type system.

use crate::source::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IntTy {
    pub bits: u8,
    pub signed: bool,
}

pub const INT: IntTy = IntTy { bits: 64, signed: true };

impl IntTy {
    pub fn name(self) -> String {
        if self == INT {
            "Int".to_string()
        } else {
            format!("{}{}", if self.signed { "i" } else { "u" }, self.bits)
        }
    }
    pub fn min(self) -> i128 {
        if self.signed {
            -(1i128 << (self.bits - 1))
        } else {
            0
        }
    }
    pub fn max(self) -> i128 {
        if self.signed {
            (1i128 << (self.bits - 1)) - 1
        } else {
            (1i128 << self.bits) - 1
        }
    }
    pub fn from_name(s: &str) -> Option<IntTy> {
        Some(match s {
            "Int" | "i64" => INT,
            "i8" => IntTy { bits: 8, signed: true },
            "i16" => IntTy { bits: 16, signed: true },
            "i32" => IntTy { bits: 32, signed: true },
            "u8" => IntTy { bits: 8, signed: false },
            "u16" => IntTy { bits: 16, signed: false },
            "u32" => IntTy { bits: 32, signed: false },
            "u64" => IntTy { bits: 64, signed: false },
            _ => return None,
        })
    }
}

pub type StructId = usize;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    Int(IntTy),
    Float,
    Bool,
    Str,
    None,
    List(Box<Type>),
    Struct(StructId),
    /// `Result[T]`: either a value of type T or an error message.
    Result(Box<Type>),
    /// Poison type that suppresses follow-up errors.
    Error,
}

impl Type {
    /// Copy types are duplicated on assignment; all others are owned and moved.
    pub fn is_copy(&self) -> bool {
        matches!(self, Type::Int(_) | Type::Float | Type::Bool | Type::None | Type::Error)
    }
    pub fn is_owned(&self) -> bool {
        !self.is_copy()
    }
    pub fn is_int(&self) -> bool {
        matches!(self, Type::Int(_))
    }
    pub fn is_numeric(&self) -> bool {
        matches!(self, Type::Int(_) | Type::Float)
    }
    /// Size in bytes of a value of this type stored in memory (struct field / list element).
    pub fn size(&self) -> u32 {
        match self {
            Type::Int(i) => (i.bits / 8) as u32,
            Type::Float => 8,
            Type::Bool | Type::None | Type::Error => 1,
            Type::Str | Type::List(_) | Type::Struct(_) | Type::Result(_) => 8,
        }
    }
}

#[derive(Clone, Debug)]
pub struct StructInfo {
    pub name: String,
    pub fields: Vec<(String, Type)>,
    pub span: Span,
    /// Byte offset of each field.
    pub offsets: Vec<u32>,
    /// Total heap size (multiple of 8).
    pub size: u32,
}

impl StructInfo {
    pub fn compute_layout(&mut self) {
        // Fields are laid out largest-alignment-first would shrink padding, but declaration
        // order is kept so that layouts are predictable for FFI users.
        let mut off = 0u32;
        self.offsets.clear();
        for (_, t) in &self.fields {
            let sz = t.size();
            off = off.div_ceil(sz) * sz;
            self.offsets.push(off);
            off += sz;
        }
        self.size = (off.div_ceil(8) * 8).max(8);
    }
}

pub fn display(t: &Type, structs: &[StructInfo]) -> String {
    match t {
        Type::Int(i) => i.name(),
        Type::Float => "Float".into(),
        Type::Bool => "Bool".into(),
        Type::Str => "String".into(),
        Type::None => "None".into(),
        Type::List(e) => format!("List[{}]", display(e, structs)),
        Type::Struct(id) => structs[*id].name.clone(),
        Type::Result(e) => format!("Result[{}]", display(e, structs)),
        Type::Error => "<error>".into(),
    }
}
