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

// Handle GetByIdWithReceiver / GetByIdWithReceiverLong (HBC >=97).
//   GetByIdWithReceiverLong dst, obj, cacheIdx, receiver, strIdx
// Hermes emits these *only* for ES6 `super.prop` access: `obj` is the parent
// prototype (Object.getPrototypeOf(homeObject)) and `receiver` is the distinct
// `this`. Regular property reads use GetById (receiver == object, implicit).
// We therefore reconstruct directly to `super.prop`; the parent-prototype `obj`
// register becomes dead and is cleaned up. The following call's leading `this`
// argument is dropped later by strip_hermes_this (Member callee).
pub fn handle_get_by_id_with_receiver(
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    // Property name is the last operand (string index), after dst, obj, cache,
    // receiver.
    let prop_idx = inst.operands.last()?.value.as_u32()?;

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
            object: Box::new(Expression::Value(crate::ir::Value::Super)),
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

// Handle TypeOfIs opcode: dst = whether `typeof src` is in the type bitmask.
// Operands: Reg8 dst, Reg8 src, UInt16 typeBitmask. The third operand is a set of
// type bits (HBC >=97), not a string index; decode it to a readable condition.
pub fn handle_typeof_is(
    inst: &Instruction,
    _file: &BytecodeFile,
    _resolve_strings: bool,
) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let src = reg_expr(&inst.operands, 1)?;
    let mask = inst.operands.get(2)?.value.as_u32()?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: crate::ir::builder::opcodes_flow::typeof_is_condition(src, mask),
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

// ToPropertyKey rDst, rValue: ToPropertyKey(value), the coercion Hermes runs on a
// computed property name (`obj[expr]`). The coercion is implicit in the bracket
// syntax, so the value passes through unchanged.
pub fn handle_to_property_key(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let value = reg_expr(&inst.operands, 1)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value,
    })
}

// GetByValWithReceiver rDst, rObj, rKey, rReceiver: the computed-key counterpart
// of GetByIdWithReceiver. Hermes emits a with-receiver form only for `super`
// access, where the looked-up object is the parent prototype and the receiver is
// the distinct `this`, so this reconstructs `super[key]` and drops the parent
// prototype register the same way handle_get_by_id_with_receiver does.
pub fn handle_get_by_val_with_receiver(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let key = reg_expr(&inst.operands, 2)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Member {
            object: Box::new(Expression::Value(crate::ir::Value::Super)),
            property: PropertyKey::Computed(Box::new(key)),
            optional: false,
        },
    })
}

// PutByValWithReceiver rObj, rKey, rValue, rReceiver, strict: `super[key] = value`.
// Operand order differs from the Get form, which puts the destination first.
pub fn handle_put_by_val_with_receiver(inst: &Instruction) -> Option<Statement> {
    let key = reg_expr(&inst.operands, 1)?;
    let value = reg_expr(&inst.operands, 2)?;

    Some(Statement::Assign {
        target: AssignTarget::Index {
            object: Expression::Value(crate::ir::Value::Super),
            key,
        },
        value,
    })
}

// CreatePrivateName rDst, strIdx: mint the symbol backing one `#field` of a class.
// The string operand is the field name as written, `#` included, so the register
// simply carries that name. Hermes then parks it in an environment slot, which
// makes the existing closure slot naming carry it into every method that reads
// the field.
pub fn handle_create_private_name(
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let name_idx = inst.operands.get(1)?.value.as_u32()?;

    let name = if resolve_strings {
        file.string_at(name_idx)
            .map(|e| e.value.clone())
            .unwrap_or_else(|| format!("#private{name_idx}"))
    } else {
        format!("#private{name_idx}")
    };

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Value(crate::ir::Value::Variable(name)),
    })
}

// GetOwnPrivateBySym rDst, rObj, cacheIdx, rSym: `dst = obj.#field`. The field is
// identified by the symbol register rather than a string index, so the key stays
// computed here; it prints as `obj.#field` once the register resolves to the name.
pub fn handle_get_own_private_by_sym(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let obj = reg_expr(&inst.operands, 1)?;
    let sym = reg_expr(&inst.operands, 3)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Member {
            object: Box::new(obj),
            property: PropertyKey::Computed(Box::new(sym)),
            optional: false,
        },
    })
}

// PutOwnPrivateBySym rObj, rValue, cacheIdx, rSym: `obj.#field = value`.
pub fn handle_put_own_private_by_sym(inst: &Instruction) -> Option<Statement> {
    let obj = reg_expr(&inst.operands, 0)?;
    let value = reg_expr(&inst.operands, 1)?;
    let sym = reg_expr(&inst.operands, 3)?;

    Some(Statement::Assign {
        target: AssignTarget::Index { object: obj, key: sym },
        value,
    })
}

// AddOwnPrivateBySym rObj, rSym, rValue: the same store, for the declaration that
// first installs the field on the instance. Operand order differs from the Put form.
pub fn handle_add_own_private_by_sym(inst: &Instruction) -> Option<Statement> {
    let obj = reg_expr(&inst.operands, 0)?;
    let sym = reg_expr(&inst.operands, 1)?;
    let value = reg_expr(&inst.operands, 2)?;

    Some(Statement::Assign {
        target: AssignTarget::Index { object: obj, key: sym },
        value,
    })
}

// PrivateIsIn rDst, rSym, rObj, cacheIdx: the `#field in obj` brand check.
pub fn handle_private_is_in(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let sym = reg_expr(&inst.operands, 1)?;
    let obj = reg_expr(&inst.operands, 2)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Binary {
            op: crate::ir::BinaryOp::In,
            left: Box::new(sym),
            right: Box::new(obj),
        },
    })
}
