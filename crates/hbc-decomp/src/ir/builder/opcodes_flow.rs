// Opcode handlers for control flow operations.

use super::opcodes_load::{get_reg, reg_expr};
use crate::ir::{BinaryOp, Binding, Expression, Statement};
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

// TypeOfIsTypes bitmask (HBC >=97 `TypeOfIs` / `JmpTypeOfIs`). The third operand
// is not a string index, it is a set of type bits. `object` covers both Object
// and Null because `typeof null === "object"`; every other type is a single bit.
// Values reverse engineered from hermesc output.
const TYPEOF_IS_MASKS: &[(u32, &str)] = &[
    (1, "undefined"),
    (2 | 256, "object"),
    (4, "string"),
    (8, "symbol"),
    (16, "boolean"),
    (32, "number"),
    (64, "bigint"),
    (128, "function"),
];

// Build the boolean condition a TypeOfIs / JmpTypeOfIs bitmask represents. One
// type gives `typeof x === "t"`, the set of every type but one gives
// `typeof x !== "t"`, and anything else becomes a disjunction of the matched
// types. Falls back to a raw marker for an unrecognised mask.
pub fn typeof_is_condition(src: Expression, mask: u32) -> Expression {
    use crate::ir::{BinaryOp, Constant, Expression as E, UnaryOp, Value};
    let type_of = |s: Expression| E::unary(UnaryOp::TypeOf, s);
    let str_val = |t: &str| E::Value(Value::Constant(Constant::String(t.to_string())));

    if let Some((_, t)) = TYPEOF_IS_MASKS.iter().find(|(m, _)| *m == mask) {
        return E::binary(BinaryOp::StrictEq, type_of(src), str_val(t));
    }
    let universe = TYPEOF_IS_MASKS.iter().fold(0u32, |a, (m, _)| a | m);
    let complement = universe & !mask;
    if let Some((_, t)) = TYPEOF_IS_MASKS.iter().find(|(m, _)| *m == complement) {
        return E::binary(BinaryOp::StrictNeq, type_of(src), str_val(t));
    }
    let matched: Vec<&str> = TYPEOF_IS_MASKS
        .iter()
        .filter(|(m, _)| mask & m == *m)
        .map(|(_, t)| *t)
        .collect();
    let mut it = matched.into_iter();
    let Some(first) = it.next() else {
        return E::binary(
            BinaryOp::StrictEq,
            type_of(src),
            str_val(&format!("type{mask}")),
        );
    };
    let mut cond = E::binary(BinaryOp::StrictEq, type_of(src.clone()), str_val(first));
    for t in it {
        let next = E::binary(BinaryOp::StrictEq, type_of(src.clone()), str_val(t));
        cond = E::binary(BinaryOp::LogicalOr, cond, next);
    }
    cond
}

// Handle JmpTypeOfIs opcode: branch when `typeof(reg)` is in the type bitmask.
pub fn handle_jmp_typeof_is(
    inst: &Instruction,
    format: &BytecodeFormat,
    _file: &BytecodeFile,
) -> Option<FlowResult> {
    let target = get_jump_target(inst, format)?;
    let src = reg_expr(&inst.operands, 1)?;
    let fallthrough = inst.offset.wrapping_add(inst.length);

    let mask = inst.operands.get(2)?.value.as_u32()?;
    let condition = typeof_is_condition(src, mask);

    Some(FlowResult::Branch {
        condition,
        target,
        fallthrough,
    })
}

// Handle JmpBuiltinIs/JmpBuiltinIsNot opcodes. The second operand is an index
// into this version's builtin table, not a typeof type id: hermesc guards a
// `f.call(...)` / `f.apply(...)` site with `JmpBuiltinIs functionPrototypeCall`
// so the fast path can call `f` directly when `call` is the intrinsic. The
// condition is spelled `f.call === HermesBuiltin.functionPrototypeCall`; the
// fold that drops the fast path lives in `transforms::optimize::builtin_guard`.
pub fn handle_jmp_builtin_is(
    name: &str,
    inst: &Instruction,
    format: &BytecodeFormat,
    version: u32,
) -> Option<FlowResult> {
    let target = get_jump_target(inst, format)?;
    let builtin_idx = inst.operands.get(1)?.value.as_u32()?;
    let src = reg_expr(&inst.operands, 2)?;
    let fallthrough = inst.offset.wrapping_add(inst.length);

    let op = if name.contains("Not") {
        crate::ir::BinaryOp::StrictNeq
    } else {
        crate::ir::BinaryOp::StrictEq
    };

    let condition = Expression::binary(op, src, builtin_ref_expr(builtin_idx, version));

    Some(FlowResult::Branch {
        condition,
        target,
        fallthrough,
    })
}

// The expression that names builtin `idx` of this version's table, as the
// `JmpBuiltinIs` guard compares against it. An index the table does not know
// keeps a placeholder that says so instead of a wrong name.
pub fn builtin_ref_expr(idx: u32, version: u32) -> Expression {
    let table = crate::opcode::builtins_for_version(version);
    match table.get(idx as usize) {
        Some(name) => crate::ir::builder::opcodes_call::builtin_name_to_expr(name),
        None => Expression::Unknown {
            opcode: format!("builtin{idx}"),
            operands: vec![],
        },
    }
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
        target: crate::ir::AssignTarget::Binding(Binding::Register(dst)),
        value: ctor_return,
    }))
}

// Handle Debugger opcode.
pub fn handle_debugger() -> Option<FlowResult> {
    Some(FlowResult::Statement(Statement::Debugger))
}

// ThrowIfThisInitialized checks that a derived constructor has not already run
// `super()`. ProfilePoint marks an instrumentation site. Neither has a JS form:
// the first is implied by `super()` itself, the second is not source at all.
// Dropping them beats leaving an unhandled-opcode comment behind.
pub fn handle_ignored_guard() -> Option<FlowResult> {
    Some(FlowResult::Noop)
}

// ThrowIfUndefined rDst, rValue: throws a ReferenceError when the value is
// undefined, otherwise moves it. That is the temporal dead zone check Hermes
// emits for a `let`/`const` read before its declaration. The guard is implicit in
// the declaration itself, so only the move is reconstructed.
pub fn handle_throw_if_undefined(inst: &Instruction) -> Option<FlowResult> {
    let dst = get_reg(&inst.operands, 0)?;
    let value = reg_expr(&inst.operands, 1)?;
    Some(FlowResult::Statement(Statement::Assign {
        target: crate::ir::AssignTarget::Binding(Binding::Register(dst)),
        value,
    }))
}

// Handle Catch opcode.
pub fn handle_catch(inst: &Instruction) -> Option<FlowResult> {
    let dst = get_reg(&inst.operands, 0)?;
    Some(FlowResult::Statement(Statement::Assign {
        target: crate::ir::AssignTarget::Binding(Binding::Register(dst)),
        value: Expression::Value(crate::ir::Value::Binding(Binding::Variable(
            "__exception".to_string(),
        ))),
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
        target: crate::ir::AssignTarget::Binding(Binding::Register(dst)),
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
