// Opcode handlers for call and construct operations.

use super::opcodes_load::{get_reg, reg_expr};
use crate::ir::{AssignTarget, Expression, Statement, Value};
use crate::{BytecodeFile, Instruction};

/// Upper bound for a call's argument count when pre-allocating. `arg_count`
/// comes from a u32 operand; a corrupt value would otherwise abort the process
/// in `Vec::with_capacity`. Real call sites have far fewer arguments than this.
const MAX_CALL_ARGS: usize = 1 << 16;

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
pub fn handle_call(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let callee = reg_expr(&inst.operands, 1)?;
    let arg_count = inst.operands.get(2)?.value.as_u32()? as usize;

    // Arguments are in registers dst-arg_count to dst-1.
    let mut arguments = Vec::with_capacity(arg_count.min(MAX_CALL_ARGS));
    if arg_count > 0 && dst >= arg_count as u32 {
        let first_arg = dst - arg_count as u32;
        for i in 0..arg_count {
            arguments.push(Expression::Value(Value::Register(first_arg + i as u32)));
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

// Handle Construct opcode.
pub fn handle_construct(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let callee = reg_expr(&inst.operands, 1)?;
    let arg_count = inst.operands.get(2)?.value.as_u32()? as usize;

    // Arguments are in registers dst-arg_count to dst-1.
    let mut arguments = Vec::with_capacity(arg_count.min(MAX_CALL_ARGS));
    if arg_count > 0 && dst >= arg_count as u32 {
        let first_arg = dst - arg_count as u32;
        for i in 0..arg_count {
            arguments.push(Expression::Value(Value::Register(first_arg + i as u32)));
        }
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

// Handle CallBuiltin opcode.
pub fn handle_call_builtin(inst: &Instruction) -> Option<Statement> {
    let dst = get_reg(&inst.operands, 0)?;
    let builtin_idx = inst.operands.get(1)?.value.as_u32()?;
    let arg_count = inst.operands.get(2)?.value.as_u32()? as usize;

    // Arguments are in registers dst-arg_count to dst-1.
    let mut arguments = Vec::with_capacity(arg_count.min(MAX_CALL_ARGS));
    if arg_count > 0 && dst >= arg_count as u32 {
        let first_arg = dst - arg_count as u32;
        for i in 0..arg_count {
            arguments.push(Expression::Value(Value::Register(first_arg + i as u32)));
        }
    }

    // Hermes builtin index table (from Builtins.def).
    // Indices 0-41  = static native builtins (only with -fstatic-builtins).
    //   OBJECT entries (0, 4, 7, 28, 40) return the constructor/namespace.
    //   METHOD entries are the callable static methods.
    // Indices 42-56 = private compiler helpers (HermesBuiltin_*).
    // Index 57     = JS builtin (spawnAsync).

    // Special semantic handling: exponentiationOperator → binary **
    if builtin_idx == 54 && arguments.len() >= 3 {
        return Some(Statement::Assign {
            target: AssignTarget::Register(dst),
            value: Expression::Binary {
                op: crate::ir::BinaryOp::Exp,
                left: Box::new(arguments[1].clone()),
                right: Box::new(arguments[2].clone()),
            },
        });
    }

    let builtin_name = match builtin_idx {
        // Static native builtins (indices 0-41, only with -fstatic-builtins)
        0 => "Array",
        1 => "Array.isArray",
        2 => "Date.UTC",
        3 => "Date.parse",
        4 => "JSON",
        5 => "JSON.parse",
        6 => "JSON.stringify",
        7 => "Math",
        8 => "Math.abs",
        9 => "Math.acos",
        10 => "Math.asin",
        11 => "Math.atan",
        12 => "Math.atan2",
        13 => "Math.ceil",
        14 => "Math.cos",
        15 => "Math.exp",
        16 => "Math.floor",
        17 => "Math.hypot",
        18 => "Math.imul",
        19 => "Math.log",
        20 => "Math.max",
        21 => "Math.min",
        22 => "Math.pow",
        23 => "Math.round",
        24 => "Math.sin",
        25 => "Math.sqrt",
        26 => "Math.tan",
        27 => "Math.trunc",
        28 => "Object",
        29 => "Object.create",
        30 => "Object.defineProperties",
        31 => "Object.defineProperty",
        32 => "Object.freeze",
        33 => "Object.getOwnPropertyDescriptor",
        34 => "Object.getOwnPropertyNames",
        35 => "Object.getPrototypeOf",
        36 => "Object.isExtensible",
        37 => "Object.isFrozen",
        38 => "Object.keys",
        39 => "Object.seal",
        40 => "String",
        41 => "String.fromCharCode",
        // Private compiler builtins (indices 42-57)
        // Remap builtins with clean JS equivalents:
        42 => "Object.setPrototypeOf",      // silentSetPrototypeOf
        49 => "Object.assign",              // copyDataProperties
        53 => "Object.assign",              // exportAll
        // requireFast → just require
        43 => {
            return Some(Statement::Assign {
                target: AssignTarget::Register(dst),
                value: Expression::Call {
                    callee: Box::new(Expression::Value(Value::Variable("require".to_string()))),
                    arguments,
                },
            });
        }
        // Builtins with no clean JS equivalent — keep as HermesBuiltin.X
        // (protected from renaming by Value::Global chain in builtin_name_to_expr)
        44 => "HermesBuiltin.getTemplateObject",
        45 => "HermesBuiltin.ensureObject",
        46 => "HermesBuiltin.getMethod",
        47 => "HermesBuiltin.throwTypeError",
        48 => "HermesBuiltin.generatorSetDelegated",
        50 => "HermesBuiltin.copyRestArgs",
        51 => "HermesBuiltin.arraySpread",
        52 => "HermesBuiltin.apply",
        54 => "HermesBuiltin.exponentiationOperator",
        55 => "HermesBuiltin.initRegexNamedGroups",
        56 => "HermesBuiltin.getOriginalNativeErrorConstructor",
        57 => "HermesBuiltin.spawnAsync",
        _ => {
            return Some(Statement::Assign {
                target: AssignTarget::Register(dst),
                value: Expression::Call {
                    callee: Box::new(Expression::Value(Value::Variable(format!(
                        "__builtin{builtin_idx}"
                    )))),
                    arguments,
                },
            })
        }
    };

    // Build proper Member expression for dotted names (e.g. "Object.defineProperty")
    // instead of a flat Variable which would get sanitized (dots → underscores)
    let callee = builtin_name_to_expr(builtin_name);

    Some(Statement::Assign {
        target: AssignTarget::Register(dst),
        value: Expression::Call {
            callee: Box::new(callee),
            arguments,
        },
    })
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
