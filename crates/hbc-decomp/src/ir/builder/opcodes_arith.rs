// Opcode handlers for arithmetic and binary operations.

use super::opcodes_load::{get_reg, reg_expr};
use crate::ir::{AssignTarget, BinaryOp, Expression, Statement, UnaryOp};
use crate::Instruction;

// Handle binary arithmetic opcodes.
pub fn handle_binary_op(name: &str, inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let left = reg_expr(&inst.operands, 1)?;
    let right = reg_expr(&inst.operands, 2)?;

    let op = match name {
        "Add" | "AddN" | "AddS" => BinaryOp::Add,
        "Sub" | "SubN" => BinaryOp::Sub,
        "Mul" | "MulN" => BinaryOp::Mul,
        "Div" | "DivN" => BinaryOp::Div,
        "Mod" => BinaryOp::Mod,
        "BitAnd" => BinaryOp::BitAnd,
        "BitOr" => BinaryOp::BitOr,
        "BitXor" => BinaryOp::BitXor,
        "LShift" | "Shl" => BinaryOp::Shl,
        "RShift" | "Shr" => BinaryOp::Shr,
        "URshift" | "UShr" => BinaryOp::UShr,
        _ => return None,
    };

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::binary(op, left, right),
    })
}

// Handle comparison opcodes.
pub fn handle_comparison(name: &str, inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let left = reg_expr(&inst.operands, 1)?;
    let right = reg_expr(&inst.operands, 2)?;

    let op = match name {
        "Eq" => BinaryOp::Eq,
        "StrictEq" => BinaryOp::StrictEq,
        "Neq" => BinaryOp::Neq,
        "StrictNeq" => BinaryOp::StrictNeq,
        "Less" => BinaryOp::Lt,
        "LessEq" => BinaryOp::Le,
        "Greater" => BinaryOp::Gt,
        "GreaterEq" => BinaryOp::Ge,
        _ => return None,
    };

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::binary(op, left, right),
    })
}

// Handle unary opcodes.
pub fn handle_unary_op(name: &str, inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let operand = reg_expr(&inst.operands, 1)?;

    let op = match name {
        "Negate" => UnaryOp::Neg,
        "Not" => UnaryOp::Not,
        "BitNot" => UnaryOp::BitNot,
        "TypeOf" => UnaryOp::TypeOf,
        _ => return None,
    };

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::unary(op, operand),
    })
}

// Handle Inc/Dec opcodes.
pub fn handle_inc_dec(name: &str, inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let src = reg_expr(&inst.operands, 1)?;

    let one = Expression::constant(crate::ir::Constant::Integer(1));
    let op = if name.contains("Inc") {
        BinaryOp::Add
    } else {
        BinaryOp::Sub
    };

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::binary(op, src, one),
    })
}

// Handle type coercion opcodes.
pub fn handle_coercion(name: &str, inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let src = reg_expr(&inst.operands, 1)?;

    let value = match name {
        "ToNumber" | "ToNumeric" => {
            // +x (unary plus coerces to number)
            Expression::Unary {
                op: crate::ir::UnaryOp::Plus,
                operand: Box::new(src),
            }
        }
        "ToInt32" => {
            // (x | 0)
            Expression::binary(
                BinaryOp::BitOr,
                src,
                Expression::constant(crate::ir::Constant::Integer(0)),
            )
        }
        "ToUint32" => {
            // (x >>> 0)
            Expression::binary(
                BinaryOp::UShr,
                src,
                Expression::constant(crate::ir::Constant::Integer(0)),
            )
        }
        "AddEmptyString" => {
            // "" + x
            Expression::binary(
                BinaryOp::Add,
                Expression::constant(crate::ir::Constant::String(String::new())),
                src,
            )
        }
        "CoerceThisNS" => src, // Object(this) - simplified
        _ => return None,
    };

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value,
    })
}

// Handle instanceof/in operators.
pub fn handle_instance_in(name: &str, inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let left = reg_expr(&inst.operands, 1)?;
    let right = reg_expr(&inst.operands, 2)?;

    let op = match name {
        "InstanceOf" => BinaryOp::InstanceOf,
        "IsIn" => BinaryOp::In,
        _ => return None,
    };

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::binary(op, left, right),
    })
}
