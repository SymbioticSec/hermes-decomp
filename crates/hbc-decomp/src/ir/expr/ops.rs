use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
    Eq,
    StrictEq,
    Neq,
    StrictNeq,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    In,
    InstanceOf,
    NullishCoalesce,
    LogicalAnd,
    LogicalOr,
    Exp,
}

impl BinaryOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Mod => "%",
            BinaryOp::BitAnd => "&",
            BinaryOp::BitOr => "|",
            BinaryOp::BitXor => "^",
            BinaryOp::Shl => "<<",
            BinaryOp::Shr => ">>",
            BinaryOp::UShr => ">>>",
            BinaryOp::Eq => "==",
            BinaryOp::StrictEq => "===",
            BinaryOp::Neq => "!=",
            BinaryOp::StrictNeq => "!==",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
            BinaryOp::And | BinaryOp::LogicalAnd => "&&",
            BinaryOp::Or | BinaryOp::LogicalOr => "||",
            BinaryOp::In => "in",
            BinaryOp::InstanceOf => "instanceof",
            BinaryOp::NullishCoalesce => "??",
            BinaryOp::Exp => "**",
        }
    }

    pub fn precedence(&self) -> u8 {
        match self {
            BinaryOp::NullishCoalesce => 3,
            BinaryOp::Or | BinaryOp::LogicalOr => 4,
            BinaryOp::And | BinaryOp::LogicalAnd => 5,
            BinaryOp::BitOr => 6,
            BinaryOp::BitXor => 7,
            BinaryOp::BitAnd => 8,
            BinaryOp::Eq | BinaryOp::StrictEq | BinaryOp::Neq | BinaryOp::StrictNeq => 9,
            BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge
            | BinaryOp::In
            | BinaryOp::InstanceOf => 10,
            BinaryOp::Shl | BinaryOp::Shr | BinaryOp::UShr => 11,
            BinaryOp::Add | BinaryOp::Sub => 12,
            BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => 13,
            BinaryOp::Exp => 14,
        }
    }
}

impl fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnaryOp {
    Neg,
    Plus,
    BitNot,
    Not,
    TypeOf,
    Void,
}

impl UnaryOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            UnaryOp::Neg => "-",
            UnaryOp::Plus => "+",
            UnaryOp::BitNot => "~",
            UnaryOp::Not => "!",
            UnaryOp::TypeOf => "typeof ",
            UnaryOp::Void => "void ",
        }
    }
}

impl fmt::Display for UnaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_binary_op_precedence() {
        assert!(BinaryOp::Mul.precedence() > BinaryOp::Add.precedence());
        assert!(BinaryOp::And.precedence() > BinaryOp::Or.precedence());
    }
}
