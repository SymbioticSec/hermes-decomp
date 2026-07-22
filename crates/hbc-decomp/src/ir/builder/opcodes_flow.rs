// Opcode handlers for control flow operations.

use super::opcodes_load::{get_reg, reg_expr};
use crate::ir::{BinaryOp, Expression, Statement};
use crate::opcode::OperandType;
use crate::{BytecodeFile, BytecodeFormat, Instruction};

// Result of processing a control flow instruction.
pub enum FlowResult {
    // Regular statement, continue in same block.
    Statement(Statement),
    // Unconditional jump to target.
    Jump {
        target: u32,
    },
    // Conditional branch.
    Branch {
        condition: Expression,
        target: u32,
        fallthrough: u32,
    },
    // Return from function.
    Return(Option<Expression>),
    // Throw exception.
    Throw(Expression),
    // No-op (e.g., environment setup).
    Noop,
    // Switch statement.
    Switch {
        value: Expression,
        default: u32,
        cases: Vec<(Expression, u32)>, // (case expression, target offset)
    },
}

// Handle unconditional jump opcodes.
pub fn handle_jmp(inst: &Instruction, format: &BytecodeFormat) -> Option<FlowResult> {
    let target = get_jump_target(inst, format)?;
    Some(FlowResult::Jump { target })
}

// Handle conditional jump opcodes (JmpTrue, JmpFalse).
// Operand order: Addr (target), Reg (condition)
pub fn handle_jmp_cond(
    name: &str,
    inst: &Instruction,
    format: &BytecodeFormat,
) -> Option<FlowResult> {
    let target = get_jump_target(inst, format)?;
    let cond = reg_expr(&inst.operands, 1)?;
    let fallthrough = inst.offset.wrapping_add(inst.length);

    let condition = if name.contains("False") {
        Expression::unary(crate::ir::UnaryOp::Not, cond)
    } else {
        cond
    };

    Some(FlowResult::Branch {
        condition,
        target,
        fallthrough,
    })
}

// Handle comparison jump opcodes (JEqual, JStrictEqual, etc.).
// Operand order: Addr (target), Reg (left), Reg (right)
pub fn handle_jmp_comparison(
    name: &str,
    inst: &Instruction,
    format: &BytecodeFormat,
) -> Option<FlowResult> {
    let target = get_jump_target(inst, format)?;
    let left = reg_expr(&inst.operands, 1)?;
    let right = reg_expr(&inst.operands, 2)?;
    let fallthrough = inst.offset.wrapping_add(inst.length);

    // Strip "Long" suffix for matching
    let base_name = name.trim_end_matches("Long");

    let op = match base_name {
        "JEqual" => BinaryOp::Eq,
        "JNotEqual" => BinaryOp::Neq,
        "JStrictEqual" => BinaryOp::StrictEq,
        "JStrictNotEqual" => BinaryOp::StrictNeq,
        "JLess" | "JLessN" => BinaryOp::Lt,
        "JLessEqual" | "JLessEqualN" => BinaryOp::Le,
        "JGreater" | "JGreaterN" => BinaryOp::Gt,
        "JGreaterEqual" | "JGreaterEqualN" => BinaryOp::Ge,
        "JNotLess" | "JNotLessN" => BinaryOp::Ge,
        "JNotLessEqual" | "JNotLessEqualN" => BinaryOp::Gt,
        "JNotGreater" | "JNotGreaterN" => BinaryOp::Le,
        "JNotGreaterEqual" | "JNotGreaterEqualN" => BinaryOp::Lt,
        _ => return None,
    };

    let condition = Expression::binary(op, left, right);
    Some(FlowResult::Branch {
        condition,
        target,
        fallthrough,
    })
}

// Map Hermes type ID enum to typeof string.
pub fn typeof_id_to_string(id: u32) -> &'static str {
    match id {
        0 => "undefined",
        1 => "object",
        2 => "boolean",
        3 => "number",
        4 => "string",
        5 => "function",
        6 => "symbol",
        7 => "bigint",
        _ => "unknown",
    }
}

// Handle JmpTypeOfIs opcode: branch if typeof(reg) === typeString.
pub fn handle_jmp_typeof_is(
    inst: &Instruction,
    format: &BytecodeFormat,
    file: &BytecodeFile,
) -> Option<FlowResult> {
    let target = get_jump_target(inst, format)?;
    let src = reg_expr(&inst.operands, 1)?;
    let fallthrough = inst.offset.wrapping_add(inst.length);

    let type_idx = inst.operands.get(2)?.value.as_u32()?;
    let type_str = file
        .string_at(type_idx)
        .map(|e| e.value.clone())
        .unwrap_or_else(|| format!("type{type_idx}"));

    let condition = Expression::binary(
        crate::ir::BinaryOp::StrictEq,
        Expression::unary(crate::ir::UnaryOp::TypeOf, src),
        Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::String(type_str))),
    );

    Some(FlowResult::Branch {
        condition,
        target,
        fallthrough,
    })
}

// Handle JmpBuiltinIs/JmpBuiltinIsNot opcodes.
pub fn handle_jmp_builtin_is(
    name: &str,
    inst: &Instruction,
    format: &BytecodeFormat,
) -> Option<FlowResult> {
    let target = get_jump_target(inst, format)?;
    let type_id = inst.operands.get(1)?.value.as_u32()?;
    let src = reg_expr(&inst.operands, 2)?;
    let fallthrough = inst.offset.wrapping_add(inst.length);

    let type_str = typeof_id_to_string(type_id).to_string();

    let op = if name.contains("Not") {
        crate::ir::BinaryOp::StrictNeq
    } else {
        crate::ir::BinaryOp::StrictEq
    };

    let condition = Expression::binary(
        op,
        Expression::unary(crate::ir::UnaryOp::TypeOf, src),
        Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::String(type_str))),
    );

    Some(FlowResult::Branch {
        condition,
        target,
        fallthrough,
    })
}

// Handle Ret opcode.
pub fn handle_ret(inst: &Instruction) -> Option<FlowResult> {
    let value = reg_expr(&inst.operands, 0)?;
    Some(FlowResult::Return(Some(value)))
}

// Handle Throw opcode.
pub fn handle_throw(inst: &Instruction) -> Option<FlowResult> {
    let value = reg_expr(&inst.operands, 0)?;
    Some(FlowResult::Throw(value))
}

// Handle SelectObject opcode.
pub fn handle_select_object(inst: &Instruction) -> Option<FlowResult> {
    // Hermes `SelectObject dst, thisObject, constructorReturn`: the result of
    // `new Ctor(...)` is the constructor's return value when it is an object,
    // otherwise the freshly-created `this`. The constructor return (operand 2)
    // holds our reconstructed `new Ctor(...)` expression, so prefer it, using
    // operand 1 (the CreateThis placeholder) surfaced the instance as
    // `new.target`.
    // operand 1 is `thisObject` (the CreateThis placeholder); we only need the
    // constructor return (operand 2).
    let dst = get_reg(&inst.operands, 0)?;
    let ctor_return = reg_expr(&inst.operands, 2)?;

    Some(FlowResult::Statement(Statement::Assign {
        target: crate::ir::AssignTarget::Register(dst),
        value: ctor_return,
    }))
}

// Handle Debugger opcode.
pub fn handle_debugger() -> Option<FlowResult> {
    Some(FlowResult::Statement(Statement::Debugger))
}

// Handle Catch opcode.
pub fn handle_catch(inst: &Instruction) -> Option<FlowResult> {
    let dst = get_reg(&inst.operands, 0)?;
    Some(FlowResult::Statement(Statement::Assign {
        target: crate::ir::AssignTarget::Register(dst),
        value: Expression::Value(crate::ir::Value::Variable("__exception".to_string())),
    }))
}

// Handle JmpUndefined opcode.
pub fn handle_jmp_undefined(
    _name: &str,
    inst: &Instruction,
    format: &BytecodeFormat,
) -> Option<FlowResult> {
    let target = get_jump_target(inst, format)?;
    let val = reg_expr(&inst.operands, 1)?;
    let fallthrough = inst.offset.wrapping_add(inst.length);

    let condition = Expression::binary(
        BinaryOp::StrictEq,
        val,
        Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::Undefined)),
    );

    Some(FlowResult::Branch {
        condition,
        target,
        fallthrough,
    })
}

// Handle GetNextPName opcode (for-in iteration).
pub fn handle_get_next_pname(inst: &Instruction) -> Option<FlowResult> {
    let dst = get_reg(&inst.operands, 0)?;
    let props = reg_expr(&inst.operands, 1)?;
    let _obj = reg_expr(&inst.operands, 2)?;
    let idx = reg_expr(&inst.operands, 3)?;
    let _size = reg_expr(&inst.operands, 4)?;

    Some(FlowResult::Statement(Statement::Assign {
        target: crate::ir::AssignTarget::Register(dst),
        value: Expression::Member {
            object: Box::new(props),
            property: crate::ir::PropertyKey::Computed(Box::new(idx)),
            optional: false,
        },
    }))
}

// Get jump target offset from instruction.
pub(super) fn get_jump_target(inst: &Instruction, format: &BytecodeFormat) -> Option<u32> {
    let def = format.definitions.get(inst.opcode as usize)?;

    if !def.is_jump {
        return None;
    }

    for operand in &inst.operands {
        if matches!(operand.ty, OperandType::Addr8 | OperandType::Addr32) {
            if let Some(rel) = operand.value.as_i32() {
                let target = (inst.offset as i32).wrapping_add(rel);
                if target >= 0 {
                    return Some(target as u32);
                }
            }
        }
    }
    None
}
