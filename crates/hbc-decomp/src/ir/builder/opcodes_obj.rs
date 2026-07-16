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

// Handle CreateBaseClass / CreateDerivedClass (HBC ≥97 ES6 class opcodes).
//   CreateBaseClass    dstClass, dstHomeObject, env, funcIdx
//   CreateDerivedClass dstClass, dstHomeObject, superClass, funcIdx
// Desugar to the constructor function + its prototype so the existing
// function-and-prototype class reconstruction can pick it up:
//   class = function <ctor>(...) { ... }
//   homeObject = class.prototype
// (methods/getters are then defined on the home object by DefineOwnByVal /
// DefineOwnGetterSetterByVal that follow).
pub fn handle_create_class(
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
    derived: bool,
) -> Option<Statement> {
    let class_reg = get_reg(&inst.operands, 0)?;
    let home_reg = get_reg(&inst.operands, 1)?;
    // funcIdx is the last operand.
    let func_idx = inst.operands.last()?.value.as_u32()?;

    let func_header = file.function_headers.get(func_idx as usize);
    let name = if resolve_strings {
        func_header
            .and_then(|h| file.string_at(h.function_name()))
            .map(|e| e.value.clone())
            .filter(|n| !n.is_empty())
    } else {
        None
    };
    let class_fn = Expression::Function {
        id: crate::ir::FunctionId(func_idx),
        name,
        is_arrow: false,
        is_async: false,
        is_generator: false,
    };
    let class_assign = Statement::Assign {
        target: AssignTarget::Register(class_reg),
        value: class_fn,
    };
    let proto_assign = Statement::Assign {
        target: AssignTarget::Register(home_reg),
        value: Expression::member(
            Expression::Value(crate::ir::Value::Register(class_reg)),
            "prototype",
        ),
    };

    // For a derived class (`class B extends A`), CreateDerivedClass carries the
    // superClass in operand 3 (dst, home, env, superClass, funcIdx). We must:
    //   1. Capture the superclass register *before* `class_assign` overwrites
    //      `class_reg` (Hermes reuses the same register for dst and super), so it
    //      still reads the base class. Capturing also keeps the base class's
    //      constructor alive (an extra use) so propagation does not inline it
    //      away into an invalid `function A(){}.prototype[...]` expression.
    //   2. Emit a recognizable `__hermes_class_extends__(class, super)` marker
    //      that the class reconstruction pass turns into `extends`. Both reads
    //      resolve correctly after SSA (capture = base, class_reg = derived).
    if derived {
        if let Some(super_reg) = get_reg(&inst.operands, 3) {
            // Synthetic, collision-free temp: above physical registers, unique
            // per derived constructor. SSA renumbers it regardless.
            let super_tmp = 0xFFFF_0000u32 | (func_idx & 0xFFFF);
            let capture = Statement::Assign {
                target: AssignTarget::Register(super_tmp),
                value: Expression::Value(crate::ir::Value::Register(super_reg)),
            };
            let extends_marker = Statement::Expr(Expression::Call {
                callee: Box::new(Expression::Value(crate::ir::Value::Variable(
                    EXTENDS_MARKER.to_string(),
                ))),
                arguments: vec![
                    Expression::Value(crate::ir::Value::Register(class_reg)),
                    Expression::Value(crate::ir::Value::Register(super_tmp)),
                ],
            });
            return Some(Statement::Block(vec![
                capture,
                class_assign,
                proto_assign,
                extends_marker,
            ]));
        }
    }

    Some(Statement::Block(vec![class_assign, proto_assign]))
}

// Sentinel callee name for the synthetic `extends` marker emitted by
// CreateDerivedClass desugaring and consumed by class reconstruction
// (transforms/class_patterns). Never appears in real bytecode.
pub const EXTENDS_MARKER: &str = "__hermes_class_extends__";

// Handle NewObjectWithParent opcode → `Object.create(parent)`.
pub fn handle_new_object_with_parent(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let parent = reg_expr(&inst.operands, 1)?;

    // `Object.create(parent)`, a single genuine argument, NOT the Hermes call
    // ABI (no `this` receiver: NewObjectWithParent is its own opcode, not a Call).
    // strip_hermes_this() special-cases single-arg `Object.create` so it does not
    // mistake `parent` for an ABI receiver and drop it. (A real source
    // `Object.create(proto)` compiles to a Call with two args, receiver + proto
    //, and is stripped normally.)
    let object_create = Expression::member(
        Expression::member(Expression::Value(crate::ir::Value::Global), "Object"),
        "create",
    );
    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Call {
            callee: Box::new(object_create),
            arguments: vec![parent],
        },
    })
}

// Handle NewObjectWithBuffer opcode.
// Old format (5 operands, HBC ≤96): Reg8 dst, UInt16 prealloc, UInt16 numProps,
//   UInt16 keyIdx, UInt16 valIdx, keys and values are directly at their offsets.
// New format (3 operands, HBC ≥97): Reg8 dst, shapeId, valBufOffset, Hermes
//   added an "object shape table" (hidden-class dedup): shapeId indexes the
//   shape table to get (key buffer offset, prop count); keys live in the object
//   key buffer, values at valBufOffset in the (unified) literal value buffer.
pub fn handle_new_object_with_buffer(
    inst: &Instruction,
    file: &BytecodeFile,
    _resolve_strings: bool,
) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let mut properties = Vec::new();

    let key_vals = if inst.operands.len() >= 5 {
        // Old format: 5 operands.
        let num_props = inst.operands.get(2)?.value.as_u32()?;
        let key_offset = inst.operands.get(3)?.value.as_u32()?;
        let val_offset = inst.operands.get(4)?.value.as_u32()?;
        Some((key_offset, val_offset, num_props))
    } else if inst.operands.len() >= 3 {
        // New format: 3 operands. Resolve keys through the shape table.
        let shape_id = inst.operands.get(1)?.value.as_u32()?;
        let val_offset = inst.operands.get(2)?.value.as_u32()?;
        let shape = file.shape_at(shape_id)?;
        Some((shape.key_buffer_offset, val_offset, shape.num_props))
    } else {
        None
    };

    if let Some((key_offset, val_offset, num_props)) = key_vals {
        if let (Ok(keys), Ok(vals)) = (
            file.read_key_buffer_series(key_offset, num_props),
            file.read_value_buffer_series(val_offset, num_props),
        ) {
            for (key, val) in keys.into_iter().zip(vals.into_iter()) {
                properties.push(ObjectProperty {
                    key: literal_to_property_key(&key),
                    value: literal_to_expression(&val),
                });
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
    // Hermes `PutOwnByVal obj, value, key, enumerable`: operand 1 is the value,
    // operand 2 the (computed) key/index. (Was read swapped, yielding
    // `arr[value] = key` for array-spread tails such as `[...a, 4, 5]`.)
    let obj = reg_expr(&inst.operands, 0)?;
    let value = reg_expr(&inst.operands, 1)?;
    let key = reg_expr(&inst.operands, 2)?;

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
    // Allocates the constructor's `this`. The real instance is produced by the
    // following Construct + SelectObject, which overwrites this register, so this
    // assignment is a placeholder that later cleanup drops. (operands 1/2 are the
    // prototype and closure, not needed here.)
    let dst = get_reg(&inst.operands, 0)?;
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
