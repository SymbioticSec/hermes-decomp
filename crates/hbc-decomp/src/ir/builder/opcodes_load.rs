// Opcode handlers for load/store operations.

use crate::ir::{AssignTarget, Constant, Expression, Statement, Value};
use crate::{BytecodeFile, Instruction};

// Handle load constant opcodes.
pub fn handle_load_const(
    name: &str,
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;

    let value = match name {
        "LoadConstUndefined" => Expression::constant(Constant::Undefined),
        "LoadConstNull" => Expression::constant(Constant::Null),
        "LoadConstTrue" => Expression::constant(Constant::Bool(true)),
        "LoadConstFalse" => Expression::constant(Constant::Bool(false)),
        "LoadConstZero" => Expression::constant(Constant::Integer(0)),
        "LoadConstEmpty" => Expression::constant(Constant::Undefined), // Empty slot
        "LoadConstUInt8" => {
            let val = inst.operands.get(1)?.value.as_u32()? as i32;
            Expression::constant(Constant::Integer(val))
        }
        "LoadConstInt" => {
            let val = inst.operands.get(1)?.value.as_i32()?;
            Expression::constant(Constant::Integer(val))
        }
        "LoadConstDouble" => {
            let val = match inst.operands.get(1)?.value {
                crate::opcode::OperandValue::F64(f) => f,
                _ => return None,
            };
            Expression::constant(Constant::Number(val))
        }
        "LoadConstString" | "LoadConstStringLongIndex" => {
            let idx = inst.operands.get(1)?.value.as_u32()?;
            if resolve_strings {
                let s = file
                    .string_at(idx)
                    .map(|e| e.value.clone())
                    .unwrap_or_default();
                Expression::constant(Constant::String(s))
            } else {
                Expression::constant(Constant::String(format!("<string:{idx}>")))
            }
        }
        "LoadConstBigInt" | "LoadConstBigIntLongIndex" => {
            let idx = inst.operands.get(1)?.value.as_u32()?;
            let value = file.bigint_at(idx).unwrap_or_else(|| format!("{idx}"));
            Expression::constant(Constant::BigInt(value))
        }
        _ => return None,
    };

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value,
    })
}

// Handle Mov opcodes.
pub fn handle_mov(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let src = get_reg(&inst.operands, 1)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Value(Value::Register(src)),
    })
}

// Handle LoadParam opcodes.
pub fn handle_load_param(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let idx = inst.operands.get(1)?.value.as_u32()?;

    let value = if idx == 0 {
        Expression::Value(Value::This)
    } else {
        Expression::Value(Value::Parameter(idx - 1))
    };

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value,
    })
}

// Handle GetGlobalObject opcode.
pub fn handle_get_global(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Value(Value::Global),
    })
}

// Handle LoadThisNS opcode.
pub fn handle_load_this(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Value(Value::This),
    })
}

// Handle DeclareGlobalVar opcode.
pub fn handle_declare_global(
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
) -> Option<Statement> {
    let idx = inst.operands.first()?.value.as_u32()?;
    let name = if resolve_strings {
        file.string_at(idx)
            .map(|e| e.value.clone())
            .unwrap_or_else(|| format!("var{idx}"))
    } else {
        format!("var{idx}")
    };

    Some(Statement::var_stmt(
        name,
        Expression::constant(Constant::Undefined),
    ))
}

// Helper to get register number from operand.
pub fn get_reg(operands: &[crate::opcode::Operand], idx: usize) -> Option<u32> {
    operands.get(idx)?.value.as_u32()
}

// Helper to get register as expression.
pub fn reg_expr(operands: &[crate::opcode::Operand], idx: usize) -> Option<Expression> {
    Some(Expression::Value(Value::Register(get_reg(operands, idx)?)))
}
