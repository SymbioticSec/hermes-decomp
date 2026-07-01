// Opcode handlers for call and construct operations.

use super::opcodes_load::{get_reg, reg_expr};
use crate::ir::{AssignTarget, Expression, Statement, Value};
use crate::{BytecodeFile, Instruction};

// Upper bound for a call's argument count when pre-allocating. `arg_count`
// comes from a u32 operand; a corrupt value would otherwise abort the process
// in `Vec::with_capacity`. Real call sites have far fewer arguments than this.
const MAX_CALL_ARGS: usize = 1 << 16;

// Hermes stack-frame offset of the `this` argument for an outgoing call,
// derived from the engine's StackFrameLayout (ThisArg is at frame offset -7,
// arg0 at -8, arg1 at -9, ...). In the caller's register file these outgoing
// slots sit at the very top of the frame, so register `frame_size - 7` holds
// `this`, `frame_size - 8` holds arg0, and so on. The instruction only encodes
// (dst, callee, argCount) — the argument *registers* are implied by this layout,
// not by `dst`.
const THIS_ARG_FROM_TOP: u32 = 7;

// Resolve the argument registers (including the leading `this`) for an
// implicit-argument call/construct, with an explicit `this`-from-top offset.
// HBC ≥97 reserves an extra outgoing frame slot (for `new.target`), so
// implicit-arg calls' args sit one register lower than on HBC ≤96.
fn resolve_implicit_args_from(arg_count: usize, frame_size: u32, this_from_top: u32) -> Vec<Expression> {
    let mut arguments = Vec::with_capacity(arg_count.min(MAX_CALL_ARGS));
    if frame_size < this_from_top {
        return arguments; // malformed / no room for the call frame
    }
    for i in 0..arg_count.min(MAX_CALL_ARGS) {
        match (frame_size - this_from_top).checked_sub(i as u32) {
            Some(reg) => arguments.push(Expression::Value(Value::Register(reg))),
            None => break,
        }
    }
    arguments
}

// HBC version at which construct-style calls gained an extra `new.target`
// outgoing frame slot, shifting their implicit args down by one register.
const NEW_TARGET_FRAME_SLOT_MIN_VERSION: u32 = 97;

// Handle Call1, Call2, Call3, Call4 opcodes (fixed argument count).
pub fn handle_call_fixed(name: &str, inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let callee = reg_expr(&inst.operands, 1)?;

    let arg_count = match name {
        "Call1" => 1,
        "Call2" => 2,
        "Call3" => 3,
        "Call4" => 4,
        _ => return None,
    };

    // Arguments start at operand index 2
    let mut arguments = Vec::with_capacity(arg_count);
    for i in 0..arg_count {
        if let Some(arg) = reg_expr(&inst.operands, 2 + i) {
            arguments.push(arg);
        }
    }

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Call {
            callee: Box::new(callee),
            arguments,
        },
    })
}

// Handle Call and CallLong opcodes (variable argument count).
pub fn handle_call(inst: &Instruction, frame_size: u32, version: u32) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let callee = reg_expr(&inst.operands, 1)?;
    let arg_count = inst.operands.get(2)?.value.as_u32()? as usize;

    // Argument registers are implied by the Hermes frame layout, not by `dst`.
    // HBC ≥97 shifted the implicit-call frame down by one slot.
    let this_from_top = if version >= NEW_TARGET_FRAME_SLOT_MIN_VERSION {
        THIS_ARG_FROM_TOP + 1
    } else {
        THIS_ARG_FROM_TOP
    };
    let arguments = resolve_implicit_args_from(arg_count, frame_size, this_from_top);

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Call {
            callee: Box::new(callee),
            arguments,
        },
    })
}

// Handle Construct opcode.
pub fn handle_construct(inst: &Instruction, frame_size: u32, version: u32) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let callee = reg_expr(&inst.operands, 1)?;
    let arg_count = inst.operands.get(2)?.value.as_u32()? as usize;

    // Argument registers are implied by the Hermes frame layout. arg[0] is the
    // construct's `this` (the freshly-created object), which is not a source-level
    // argument — drop it so `new Ctor(a, b)` keeps only the explicit args.
    // HBC ≥97 reserves an extra `new.target` outgoing slot, shifting args down.
    let this_from_top = if version >= NEW_TARGET_FRAME_SLOT_MIN_VERSION {
        THIS_ARG_FROM_TOP + 1
    } else {
        THIS_ARG_FROM_TOP
    };
    let mut arguments = resolve_implicit_args_from(arg_count, frame_size, this_from_top);
    if !arguments.is_empty() {
        arguments.remove(0);
    }

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::New {
            callee: Box::new(callee),
            arguments,
        },
    })
}

// Handle CreateClosure opcode.
pub fn handle_create_closure(
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    // Environment is operand 1
    let func_idx = inst.operands.get(2)?.value.as_u32()?;

    let func_header = file.function_headers.get(func_idx as usize);

    let name = if resolve_strings {
        func_header
            .and_then(|h| file.string_at(h.function_name()))
            .map(|e| e.value.clone())
            .filter(|n| !n.is_empty())
    } else {
        None
    };

    // Detect arrow functions using bytecode flags
    let is_arrow = func_header.map(|h| h.is_likely_arrow()).unwrap_or(false);

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Function {
            id: crate::ir::FunctionId(func_idx),
            name,
            is_arrow,
            is_async: false,
            is_generator: false,
        },
    })
}

// Handle CreateAsyncClosure opcode.
pub fn handle_create_async_closure(
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let func_idx = inst.operands.get(2)?.value.as_u32()?;

    let func_header = file.function_headers.get(func_idx as usize);

    let name = if resolve_strings {
        func_header
            .and_then(|h| file.string_at(h.function_name()))
            .map(|e| e.value.clone())
            .filter(|n| !n.is_empty())
    } else {
        None
    };

    let is_arrow = func_header.map(|h| h.is_likely_arrow()).unwrap_or(false);

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Function {
            id: crate::ir::FunctionId(func_idx),
            name,
            is_arrow,
            is_async: true,
            is_generator: false,
        },
    })
}

// Handle CreateGeneratorClosure opcode.
pub fn handle_create_generator_closure(
    inst: &Instruction,
    file: &BytecodeFile,
    resolve_strings: bool,
) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let func_idx = inst.operands.get(2)?.value.as_u32()?;

    let func_header = file.function_headers.get(func_idx as usize);

    let name = if resolve_strings {
        func_header
            .and_then(|h| file.string_at(h.function_name()))
            .map(|e| e.value.clone())
            .filter(|n| !n.is_empty())
    } else {
        None
    };

    let is_arrow = func_header.map(|h| h.is_likely_arrow()).unwrap_or(false);

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Function {
            id: crate::ir::FunctionId(func_idx),
            name,
            is_arrow,
            is_async: false,
            is_generator: true,
        },
    })
}

// Handle CallBuiltin opcode. The builtin index -> name mapping is VERSION
// SPECIFIC (parsed from each Hermes release's Builtins.def), so it is resolved
// from the per-version table rather than hardcoded.
pub fn handle_call_builtin(inst: &Instruction, frame_size: u32, version: u32) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let builtin_idx = inst.operands.get(1)?.value.as_u32()?;
    let arg_count = inst.operands.get(2)?.value.as_u32()? as usize;

    // Argument registers follow the Hermes frame layout (like Call/Construct).
    // arg[0] is the `this` slot (undefined for builtins); drop it so the call
    // keeps only the real arguments. HBC ≥97 shifted the implicit-call frame
    // down by one slot (same change that affects Construct).
    let this_from_top = if version >= NEW_TARGET_FRAME_SLOT_MIN_VERSION {
        THIS_ARG_FROM_TOP + 1
    } else {
        THIS_ARG_FROM_TOP
    };
    let mut arguments = resolve_implicit_args_from(arg_count, frame_size, this_from_top);
    if !arguments.is_empty() {
        arguments.remove(0);
    }

    // Resolve the builtin name from this version's table. The raw name is e.g.
    // "Math.acos", "HermesInternal.ensureObject" (old) or "HermesBuiltin.ensureObject"
    // (new) / "HermesBuiltin.silentSetPrototypeOf".
    let table = crate::opcode::builtins_for_version(version);
    let raw = table.get(builtin_idx as usize).map(|s| s.as_str());
    let assign = |dst, value| Some(Statement::Assign { target: AssignTarget::Register(dst), value });

    // Name-based semantic rewrites (work across versions where the index differs).
    let suffix = raw.and_then(|n| n.rsplit('.').next()).unwrap_or("");
    match suffix {
        // x ** y
        "exponentiationOperator" if arguments.len() >= 2 => {
            return assign(dst, Expression::Binary {
                op: crate::ir::BinaryOp::Exp,
                left: Box::new(arguments[0].clone()),
                right: Box::new(arguments[1].clone()),
            });
        }
        // require(...)
        "requireFast" => {
            return assign(dst, Expression::Call {
                callee: Box::new(Expression::Value(Value::Variable("require".to_string()))),
                arguments,
            });
        }
        _ => {}
    }

    // Clean JS equivalents for some private helpers.
    let name: String = match suffix {
        "silentSetPrototypeOf" => "Object.setPrototypeOf".to_string(),
        "copyDataProperties" | "exportAll" => "Object.assign".to_string(),
        _ => match raw {
            Some(n) => n.to_string(),
            None => {
                // Unknown index for this version: keep a debuggable placeholder.
                return assign(dst, Expression::Call {
                    callee: Box::new(Expression::Value(Value::Variable(format!("__builtin{builtin_idx}")))),
                    arguments,
                });
            }
        },
    };

    // Build proper Member expression for dotted names (e.g. "Object.defineProperty")
    // instead of a flat Variable which would get sanitized (dots → underscores).
    let callee = builtin_name_to_expr(&name);
    assign(dst, Expression::Call { callee: Box::new(callee), arguments })
}

// Convert a dotted builtin name like "Object.defineProperty" into a proper
// Member expression tree using Value::Global to prevent variable renaming.
// The codegen simplifies globalThis.Object → Object via is_builtin_global().
fn builtin_name_to_expr(name: &str) -> Expression {
    if let Some(dot_pos) = name.find('.') {
        let obj_name = &name[..dot_pos];
        let prop_name = &name[dot_pos + 1..];
        Expression::Member {
            object: Box::new(Expression::member(
                Expression::Value(Value::Global),
                obj_name,
            )),
            property: crate::ir::PropertyKey::Ident(prop_name.to_string()),
            optional: false,
        }
    } else {
        Expression::member(
            Expression::Value(Value::Global),
            name,
        )
    }
}

// Handle GetBuiltinClosure opcode.
pub fn handle_get_builtin_closure(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let builtin_idx = inst.operands.get(1)?.value.as_u32()?;

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Unknown {
            opcode: format!("builtin{builtin_idx}"),
            operands: vec![],
        },
    })
}

// Handle CallRequire opcode.
pub fn handle_call_require(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;

    // Operand 2 contains the module ID
    let module_arg = if let Some(op) = inst.operands.get(2) {
        match op.value {
            crate::opcode::OperandValue::U8(v) => Some(v as u32),
            crate::opcode::OperandValue::U16(v) => Some(v as u32),
            crate::opcode::OperandValue::U32(v) => Some(v),
            _ => None,
        }
    } else {
        None
    };

    let arg_expr = if let Some(id) = module_arg {
        Expression::Value(Value::Constant(crate::ir::Constant::Integer(id as i32)))
    } else {
        // Fallback if operand is not an immediate integer?
        // Usually CallRequire has immediate module ID.
        // If not, we might need reg_expr(inst.operands, 2)
        if let Some(expr) = reg_expr(&inst.operands, 2) {
            expr
        } else {
            Expression::Value(Value::Constant(crate::ir::Constant::Integer(-1)))
        }
    };

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Call {
            callee: Box::new(Expression::Value(Value::Variable("require".to_string()))),
            arguments: vec![arg_expr],
        },
    })
}
