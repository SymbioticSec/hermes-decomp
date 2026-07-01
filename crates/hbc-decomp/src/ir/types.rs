use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockId(pub u32);

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "B{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FunctionId(pub u32);

impl fmt::Display for FunctionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "F{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Constant {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    Integer(i32),
    String(String),
    BigInt(String),
}

impl fmt::Display for Constant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Constant::Undefined => write!(f, "undefined"),
            Constant::Null => write!(f, "null"),
            Constant::Bool(b) => write!(f, "{b}"),
            Constant::Number(n) if n.is_nan() => write!(f, "NaN"),
            Constant::Number(n) if n.is_infinite() => {
                write!(f, "{}Infinity", if n.is_sign_negative() { "-" } else { "" })
            }
            Constant::Number(n) => write!(f, "{n}"),
            Constant::Integer(i) => write!(f, "{i}"),
            Constant::String(s) => write!(f, "\"{}\"", crate::util::escape_js_string_bare(s)),
            Constant::BigInt(s) => write!(f, "{s}n"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Register(u32),
    Variable(String),
    Constant(Constant),
    This,
    Global,
    Parameter(u32),
    ClosureVar { level: u32, slot: u32 },
    Arguments,
    NewTarget,
    // The `super` keyword (ES6 class). Only valid inside a class method body;
    // produced when reconstructing `super.method()` from Hermes
    // GetByIdWithReceiver opcodes (emitted exclusively for super property access).
    Super,
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Register(r) => write!(f, "r{r}"),
            Value::Variable(name) => {
                // Sanitize identifiers (handles @@symbols, invalid chars, etc.)
                let sanitized = crate::util::sanitize_identifier(name);
                write!(f, "{sanitized}")
            }
            Value::Constant(c) => write!(f, "{c}"),
            Value::This => write!(f, "this"),
            Value::Global => write!(f, "globalThis"),
            Value::Parameter(i) => write!(f, "arg{i}"),
            Value::ClosureVar { level, slot } => {
                if *level == 0 {
                    write!(f, "closure_{slot}")
                } else {
                    write!(f, "outer{level}_{slot}")
                }
            }
            Value::Arguments => write!(f, "arguments"),
            Value::NewTarget => write!(f, "new.target"),
            Value::Super => write!(f, "super"),
        }
    }
}

