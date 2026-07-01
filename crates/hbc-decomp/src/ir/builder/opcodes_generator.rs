// Opcode handlers for generator operations.

use super::opcodes_flow::FlowResult;
use super::opcodes_load::{get_reg, reg_expr};
use crate::ir::{Expression, Statement};

// Handle StartGenerator opcode.
pub fn handle_start_generator() -> Option<FlowResult> {
    Some(FlowResult::Statement(Statement::Comment(
        "StartGenerator".to_string(),
    )))
}

// Handle ResumeGenerator opcode.
pub fn handle_resume_generator(inst: &crate::Instruction) -> Option<FlowResult> {
    // ResumeGenerator dst, gen
    let dst = get_reg(&inst.operands, 0)?;
    let gen = reg_expr(&inst.operands, 1)?;
    Some(FlowResult::Statement(Statement::Assign {
        target: crate::ir::AssignTarget::Register(dst),
        value: Expression::Call {
            callee: Box::new(Expression::Member {
                object: Box::new(gen),
                property: crate::ir::PropertyKey::Ident("resume".to_string()),
                optional: false,
            }),
            arguments: vec![],
        },
    }))
}

// Handle CreateGenerator opcode.
// CreateGenerator dst, env, funcIdx — creates a generator object wrapping the inner function.
pub fn handle_create_generator(inst: &crate::Instruction) -> Option<FlowResult> {
    let dst = get_reg(&inst.operands, 0)?;
    // Third operand is the function index of the inner generator body
    let func_idx = inst.operands.get(2).and_then(|o| o.value.as_u32());
    if let Some(func_idx) = func_idx {
        // Emit as a Function expression so the closure context can track the parent-child relationship
        Some(FlowResult::Statement(Statement::Assign {
            target: crate::ir::AssignTarget::Register(dst),
            value: Expression::Function {
                id: crate::ir::FunctionId(func_idx),
                name: None,
                is_arrow: false,
                is_async: false,
                is_generator: true,
            },
        }))
    } else {
        // Fallback for older bytecode without funcIdx operand
        let env = get_reg(&inst.operands, 1)?;
        Some(FlowResult::Statement(Statement::Assign {
            target: crate::ir::AssignTarget::Register(dst),
            value: Expression::Call {
                callee: Box::new(Expression::Value(crate::ir::Value::Variable(
                    "CreateGenerator".to_string(),
                ))),
                arguments: vec![Expression::Value(crate::ir::Value::Register(env))],
            },
        }))
    }
}

// Handle CompleteGenerator opcode.
pub fn handle_complete_generator(_inst: &crate::Instruction) -> Option<FlowResult> {
    // CompleteGenerator only marks the generator as finished; it is always
    // immediately followed by a `Ret` of the final value (`undefined`). Emitting
    // a `Return(None)` here ended the block early, so structure recovery fell
    // through to that trailing `Ret <undef-reg>` in a separate block and rendered
    // a dangling `return tmp`. Treat it as a no-op and let the real `Ret` return.
    Some(FlowResult::Noop)
}

// Handle SaveGenerator opcode.
// SaveGenerator saves the current state and specifies where to resume.
// The next instruction after SaveGenerator is typically a Ret that yields the value.
// Operand: Addr8 or Addr32 - the resume address (relative offset).
pub fn handle_save_generator(inst: &crate::Instruction, _format: &crate::BytecodeFormat) -> Option<FlowResult> {
    // Get the resume address from the operand
    let resume_offset = match inst.operands.first()?.value {
        crate::opcode::OperandValue::I8(v) => v as i32,
        crate::opcode::OperandValue::I32(v) => v,
        _ => return None,
    };

    let resume_addr = (inst.offset as i32).wrapping_add(resume_offset) as u32;

    // SaveGenerator creates a yield point
    // We mark this with a special statement that will be transformed later
    Some(FlowResult::Statement(Statement::Comment(format!(
        "__yield_point__:{resume_addr}"
    ))))
}
