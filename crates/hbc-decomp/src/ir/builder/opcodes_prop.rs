// Opcode handlers for property access operations.

use super::opcodes_load::{get_reg, reg_expr};
use crate::ir::{AssignTarget, Expression, PropertyKey, Statement};
use crate::{BytecodeFile, Instruction};

// Handle GetById opcodes.
pub fn handle_get_by_id(
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let obj = reg_expr(&inst.operands, 1)?;

    // Property name is typically operand 3 (after cache index)
    let prop_idx = if inst.operands.len() > 3 {
        inst.operands.get(3)?.value.as_u32()?
    } else if inst.operands.len() > 2 {
        inst.operands.get(2)?.value.as_u32()?
    } else {
        return None;
    };

    let prop_name = if resolve_strings {
        file.string_at(prop_idx)
            .map(|e| e.value.clone())
            .unwrap_or_else(|| format!("prop{prop_idx}"))
    } else {
        format!("prop{prop_idx}")
    };

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Member {
            object: Box::new(obj),
            property: PropertyKey::Ident(prop_name),
            optional: false,
        },
    })
}

// Handle TryGetById opcodes (with optional chaining semantics).
pub fn handle_try_get_by_id(
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let obj = reg_expr(&inst.operands, 1)?;

    let prop_idx = if inst.operands.len() > 3 {
        inst.operands.get(3)?.value.as_u32()?
    } else if inst.operands.len() > 2 {
        inst.operands.get(2)?.value.as_u32()?
    } else {
        return None;
    };

    let prop_name = if resolve_strings {
        file.string_at(prop_idx)
            .map(|e| e.value.clone())
            .unwrap_or_else(|| format!("prop{prop_idx}"))
    } else {
        format!("prop{prop_idx}")
    };

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Member {
            object: Box::new(obj),
            property: PropertyKey::Ident(prop_name),
            optional: false, // TryGetById doesn't throw, but isn't ?. either
        },
    })
}

// Handle PutById opcodes.
pub fn handle_put_by_id(
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
) -> Option<Statement> {
    let obj = reg_expr(&inst.operands, 0)?;
    let value = reg_expr(&inst.operands, 1)?;

    let prop_idx = if inst.operands.len() > 3 {
        inst.operands.get(3)?.value.as_u32()?
    } else if inst.operands.len() > 2 {
        inst.operands.get(2)?.value.as_u32()?
    } else {
        return None;
    };

    let prop_name = if resolve_strings {
        file.string_at(prop_idx)
            .map(|e| e.value.clone())
            .unwrap_or_else(|| format!("prop{prop_idx}"))
    } else {
        format!("prop{prop_idx}")
    };

    Some(Statement::Assign {
        target: AssignTarget::Member {
            object: obj,
            property: prop_name,
        },
        value,
    })
}

// Handle GetByVal opcode.
pub fn handle_get_by_val(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let obj = reg_expr(&inst.operands, 1)?;
    let key = reg_expr(&inst.operands, 2)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Member {
            object: Box::new(obj),
            property: PropertyKey::Computed(Box::new(key)),
            optional: false,
        },
    })
}

// Handle PutByVal opcode.
pub fn handle_put_by_val(inst: &Instruction) -> Option<Statement> {
    let obj = reg_expr(&inst.operands, 0)?;
    let key = reg_expr(&inst.operands, 1)?;
    let value = reg_expr(&inst.operands, 2)?;

    Some(Statement::Assign {
        target: AssignTarget::Index { object: obj, key },
        value,
    })
}

// Handle DelByVal opcode.
pub fn handle_del_by_val(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let obj = reg_expr(&inst.operands, 1)?;
    let key = reg_expr(&inst.operands, 2)?;

    // delete obj[key]
    Some(Statement::Delete {
        target: Expression::Member {
            object: Box::new(obj),
            property: PropertyKey::Computed(Box::new(key)),
            optional: false,
        },
        result: Some(dst),
    })
}

// Handle TypeOfIs opcode: dst = (typeof src) === typeString.
// Operands: Reg8 dst, Reg8 src, UInt16 typeStringIdx
pub fn handle_typeof_is(
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let src = reg_expr(&inst.operands, 1)?;

    let type_idx = inst.operands.get(2)?.value.as_u32()?;
    let type_str = if resolve_strings {
        file.string_at(type_idx)
            .map(|e| e.value.clone())
            .unwrap_or_else(|| format!("type{type_idx}"))
    } else {
        format!("type{type_idx}")
    };

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Binary {
            op: crate::ir::BinaryOp::StrictEq,
            left: Box::new(Expression::Unary {
                op: crate::ir::UnaryOp::TypeOf,
                operand: Box::new(src),
            }),
            right: Box::new(Expression::Value(crate::ir::Value::Constant(
                crate::ir::Constant::String(type_str),
            ))),
        },
    })
}

// Handle DelById opcode.
pub fn handle_del_by_id(
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let obj = reg_expr(&inst.operands, 1)?;

    let prop_idx = inst.operands.get(2)?.value.as_u32()?;
    let prop_name = if resolve_strings {
        file.string_at(prop_idx)
            .map(|e| e.value.clone())
            .unwrap_or_else(|| format!("prop{prop_idx}"))
    } else {
        format!("prop{prop_idx}")
    };

    // delete obj.prop
    Some(Statement::Delete {
        target: Expression::Member {
            object: Box::new(obj),
            property: PropertyKey::Ident(prop_name),
            optional: false,
        },
        result: Some(dst),
    })
}
