// Opcode handlers for object and array operations.

use super::opcodes_load::{get_reg, reg_expr};
use crate::ir::{AssignTarget, Constant, Expression, ObjectProperty, PropertyKey, Statement};
use crate::{BytecodeFile, Instruction};

// Handle NewObject opcode.
pub fn handle_new_object(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Object { properties: vec![] },
    })
}

// Handle NewObjectWithParent opcode.
pub fn handle_new_object_with_parent(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let _parent = reg_expr(&inst.operands, 1)?;

    // Object.create(parent)
    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Object { properties: vec![] },
    })
}

// Handle NewObjectWithBuffer opcode.
// Old format (5 operands): Reg8 dst, UInt16 prealloc, UInt16 numProps, UInt16 keyIdx, UInt16 valIdx
// New format (3 operands): Reg8 dst, UInt16/UInt32 numProps, UInt16/UInt32 bufferIdx
pub fn handle_new_object_with_buffer(
    inst: &Instruction,
    file: &BytecodeFile,
    _resolve_strings: bool,
) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let mut properties = Vec::new();

    if inst.operands.len() >= 5 {
        // Old format: 5 operands
        let num_props = inst.operands.get(2)?.value.as_u32()?;
        let key_offset = inst.operands.get(3)?.value.as_u32()?;
        let val_offset = inst.operands.get(4)?.value.as_u32()?;

        if let (Ok(keys), Ok(vals)) = (
            file.read_key_buffer_series(key_offset, num_props),
            file.read_value_buffer_series(val_offset, num_props),
        ) {
            for (key, val) in keys.into_iter().zip(vals.into_iter()) {
                let key = literal_to_property_key(&key);
                let value = literal_to_expression(&val);
                properties.push(ObjectProperty { key, value });
            }
        }
    } else if inst.operands.len() >= 3 {
        // New format: 3 operands (numProps, bufferIdx)
        // In newer Hermes, key and value buffers share the same index
        let num_props = inst.operands.get(1)?.value.as_u32()?;
        let buf_idx = inst.operands.get(2)?.value.as_u32()?;

        if let (Ok(keys), Ok(vals)) = (
            file.read_key_buffer_series(buf_idx, num_props),
            file.read_value_buffer_series(buf_idx, num_props),
        ) {
            for (key, val) in keys.into_iter().zip(vals.into_iter()) {
                let key = literal_to_property_key(&key);
                let value = literal_to_expression(&val);
                properties.push(ObjectProperty { key, value });
            }
        }
    }

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Object { properties },
    })
}

// Handle NewArray opcode.
pub fn handle_new_array(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let size = inst.operands.get(1)?.value.as_u32()? as usize;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Array {
            elements: vec![None; size],
        },
    })
}

// Handle NewArrayWithBuffer opcode.
pub fn handle_new_array_with_buffer(inst: &Instruction, file: &BytecodeFile) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let _prealloc = inst.operands.get(1)?.value.as_u32()?;
    let static_elems = inst.operands.get(2)?.value.as_u32()?;
    let offset = inst.operands.get(3)?.value.as_u32()?;

    let mut elements = Vec::new();
    if let Ok(values) = file.read_array_buffer_series(offset, static_elems) {
        for val in values {
            elements.push(Some(literal_to_expression(&val)));
        }
    }

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Array { elements },
    })
}

// Handle PutOwnByIndex opcode.
pub fn handle_put_own_by_index(inst: &Instruction) -> Option<Statement> {
    let obj = reg_expr(&inst.operands, 0)?;
    let value = reg_expr(&inst.operands, 1)?;
    let index = inst.operands.get(2)?.value.as_u32()? as i64;

    Some(Statement::Assign {
        target: AssignTarget::Index {
            object: obj,
            key: Expression::constant(Constant::Integer(index as i32)),
        },
        value,
    })
}

// Handle GetOwnBySlotIdx opcode: dst = obj.slot[idx]
pub fn handle_get_own_by_slot(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let obj = reg_expr(&inst.operands, 1)?;
    let slot = inst.operands.get(2)?.value.as_u32()? as i64;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Member {
            object: Box::new(obj),
            property: PropertyKey::Computed(Box::new(Expression::constant(Constant::Integer(
                slot as i32,
            )))),
            optional: false,
        },
    })
}

// Handle GetByIndex opcode: dst = obj[index] (constant index)
pub fn handle_get_by_index(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let obj = reg_expr(&inst.operands, 1)?;
    let index = inst.operands.get(2)?.value.as_u32()? as i64;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Member {
            object: Box::new(obj),
            property: PropertyKey::Computed(Box::new(Expression::constant(Constant::Integer(
                index as i32,
            )))),
            optional: false,
        },
    })
}

// Handle PutOwnByVal opcode.
pub fn handle_put_own_by_val(inst: &Instruction) -> Option<Statement> {
    let obj = reg_expr(&inst.operands, 0)?;
    let key = reg_expr(&inst.operands, 1)?;
    let value = reg_expr(&inst.operands, 2)?;

    Some(Statement::Assign {
        target: AssignTarget::Index { object: obj, key },
        value,
    })
}

// Handle FastArrayLoad opcode.
pub fn handle_fast_array_load(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let arr = reg_expr(&inst.operands, 1)?;
    let idx = reg_expr(&inst.operands, 2)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Member {
            object: Box::new(arr),
            property: PropertyKey::Computed(Box::new(idx)),
            optional: false,
        },
    })
}

// Handle FastArrayStore opcode.
pub fn handle_fast_array_store(inst: &Instruction) -> Option<Statement> {
    let arr = reg_expr(&inst.operands, 0)?;
    let idx = reg_expr(&inst.operands, 1)?;
    let value = reg_expr(&inst.operands, 2)?;

    Some(Statement::Assign {
        target: AssignTarget::Index {
            object: arr,
            key: idx,
        },
        value,
    })
}

// Handle FastArrayPush opcode.
pub fn handle_fast_array_push(inst: &Instruction) -> Option<Statement> {
    let arr = reg_expr(&inst.operands, 0)?;
    let value = reg_expr(&inst.operands, 1)?;

    Some(Statement::Expr(Expression::Call {
        callee: Box::new(Expression::member(arr, "push")),
        arguments: vec![value],
    }))
}

// Handle FastArrayLength opcode.
pub fn handle_fast_array_length(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let arr = reg_expr(&inst.operands, 1)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::member(arr, "length"),
    })
}

// Convert literal value to expression.
fn literal_to_expression(lit: &crate::file::LiteralValue) -> Expression {
    match lit {
        crate::file::LiteralValue::Null => Expression::constant(Constant::Null),
        crate::file::LiteralValue::Bool(b) => Expression::constant(Constant::Bool(*b)),
        crate::file::LiteralValue::Number(n) => Expression::constant(Constant::Number(*n)),
        crate::file::LiteralValue::Integer(i) => Expression::constant(Constant::Integer(*i)),
        crate::file::LiteralValue::String(s) => Expression::constant(Constant::String(s.clone())),
        crate::file::LiteralValue::Undefined => Expression::constant(Constant::Undefined),
    }
}

// Convert literal value to property key.
fn literal_to_property_key(lit: &crate::file::LiteralValue) -> PropertyKey {
    match lit {
        crate::file::LiteralValue::String(s) => PropertyKey::Ident(s.clone()),
        crate::file::LiteralValue::Integer(i) => PropertyKey::Index(*i as i64),
        crate::file::LiteralValue::Number(n) => PropertyKey::Index(*n as i64),
        _ => PropertyKey::String(format!("{lit:?}")),
    }
}

// Handle CreateRegExp opcode.
pub fn handle_create_regexp(
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let pattern_idx = inst.operands.get(1)?.value.as_u32()?;
    let flags_idx = inst.operands.get(2)?.value.as_u32()?;

    let pattern = if resolve_strings {
        file.string_at(pattern_idx)
            .map(|e| e.value.clone())
            .unwrap_or_default()
    } else {
        format!("string{pattern_idx}")
    };

    let flags = if resolve_strings {
        file.string_at(flags_idx)
            .map(|e| e.value.clone())
            .unwrap_or_default()
    } else {
        String::new()
    };

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::RegExp { pattern, flags },
    })
}

// Handle GetArgumentsLength opcode.
pub fn handle_get_arguments_length(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::member(Expression::Value(crate::ir::Value::Arguments), "length"),
    })
}

// Handle GetArgumentsPropByVal opcode.
pub fn handle_get_arguments_prop_by_val(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let idx = reg_expr(&inst.operands, 1)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Member {
            object: Box::new(Expression::Value(crate::ir::Value::Arguments)),
            property: PropertyKey::Computed(Box::new(idx)),
            optional: false,
        },
    })
}

// Handle ReifyArguments opcode.
pub fn handle_reify_arguments(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Value(crate::ir::Value::Arguments),
    })
}

// Handle CreateThis opcode.
pub fn handle_create_this(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let _proto = reg_expr(&inst.operands, 1)?;
    let _closure = reg_expr(&inst.operands, 2)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Value(crate::ir::Value::NewTarget),
    })
}

// Handle GetNewTarget opcode.
pub fn handle_get_new_target(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Value(crate::ir::Value::NewTarget),
    })
}

// Handle IteratorBegin opcode.
pub fn handle_iterator_begin(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let source = reg_expr(&inst.operands, 1)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Call {
            callee: Box::new(Expression::Member {
                object: Box::new(source),
                property: PropertyKey::Computed(Box::new(Expression::Member {
                    object: Box::new(Expression::Value(crate::ir::Value::Variable(
                        "Symbol".to_string(),
                    ))),
                    property: PropertyKey::Ident("iterator".to_string()),
                    optional: false,
                })),
                optional: false,
            }),
            arguments: vec![],
        },
    })
}

// Handle IteratorNext opcode.
pub fn handle_iterator_next(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let iter = reg_expr(&inst.operands, 1)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Call {
            callee: Box::new(Expression::member(iter, "next")),
            arguments: vec![],
        },
    })
}

// Handle IteratorClose opcode.
pub fn handle_iterator_close(inst: &Instruction) -> Option<Statement> {
    let iter = reg_expr(&inst.operands, 0)?;
    let _ignore_inner = inst.operands.get(1);

    Some(Statement::Expr(Expression::Call {
        callee: Box::new(Expression::member(iter, "return")),
        arguments: vec![],
    }))
}

// Handle GetPNameList opcode (for-in enumeration).
pub fn handle_get_pname_list(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let obj = reg_expr(&inst.operands, 1)?;
    let _idx = reg_expr(&inst.operands, 2)?;
    let _size = reg_expr(&inst.operands, 3)?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Call {
            callee: Box::new(Expression::member(
                Expression::Value(crate::ir::Value::Variable("Object".to_string())),
                "keys",
            )),
            arguments: vec![obj],
        },
    })
}

// Handle PutOwnGetterSetterByVal opcode.
pub fn handle_put_own_getter_setter_by_val(inst: &Instruction) -> Option<Statement> {
    let obj = reg_expr(&inst.operands, 0)?;
    let key = reg_expr(&inst.operands, 1)?;
    let getter = reg_expr(&inst.operands, 2)?;
    let setter = reg_expr(&inst.operands, 3)?;
    let _enumerable = inst.operands.get(4);

    // Use globalThis.Object.defineProperty to avoid Variable("Object") being renamed
    // by var_naming passes. Value::Global is immune to rename_variables_in_stmts.
    // The codegen simplifies globalThis.Object → Object via is_builtin_global.
    // Include a dummy undefined as arguments[0] for the Hermes this-arg convention
    // (strip_hermes_this will remove it).
    Some(Statement::Expr(Expression::Call {
        callee: Box::new(Expression::member(
            Expression::member(
                Expression::Value(crate::ir::Value::Global),
                "Object",
            ),
            "defineProperty",
        )),
        arguments: vec![
            Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::Undefined)),
            obj,
            key,
            Expression::Object {
                properties: vec![
                    ObjectProperty {
                        key: PropertyKey::Ident("get".to_string()),
                        value: getter,
                    },
                    ObjectProperty {
                        key: PropertyKey::Ident("set".to_string()),
                        value: setter,
                    },
                ],
            },
        ],
    }))
}
