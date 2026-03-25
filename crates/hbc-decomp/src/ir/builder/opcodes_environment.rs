// Opcode handlers for environment/closure operations.

use super::opcodes_flow::FlowResult;
use super::opcodes_load::{get_reg, reg_expr};
use crate::ir::{Expression, Statement};

// Handle environment opcodes (mostly no-op for decompilation).
pub fn handle_create_environment(inst: &crate::Instruction) -> Option<FlowResult> {
    let _dst = get_reg(&inst.operands, 0)?;
    // CreateEnvironment is a no-op in terms of visible JS code
    Some(FlowResult::Noop)
}

// Handle GetEnvironment opcode.
pub fn handle_get_environment(inst: &crate::Instruction) -> Option<FlowResult> {
    let _dst = get_reg(&inst.operands, 0)?;
    let _level = inst.operands.get(1)?.value.as_u32()?;
    Some(FlowResult::Noop)
}

// Handle LoadFromEnvironment opcode.
pub fn handle_load_from_environment(inst: &crate::Instruction) -> Option<FlowResult> {
    let dst = get_reg(&inst.operands, 0)?;
    let _env = get_reg(&inst.operands, 1)?;
    let slot = inst.operands.get(2)?.value.as_u32()?;

    // Represent as loading from a closure variable
    Some(FlowResult::Statement(Statement::Assign {
        target: crate::ir::AssignTarget::Register(dst),
        value: Expression::Value(crate::ir::Value::ClosureVar { level: 0, slot }),
    }))
}

// Handle StoreToEnvironment opcode.
pub fn handle_store_to_environment(inst: &crate::Instruction) -> Option<FlowResult> {
    let _env = get_reg(&inst.operands, 0)?;
    let slot = inst.operands.get(1)?.value.as_u32()?;
    let value = reg_expr(&inst.operands, 2)?;

    Some(FlowResult::Statement(Statement::Assign {
        target: crate::ir::AssignTarget::ClosureVar { level: 0, slot },
        value,
    }))
}

// Handle StoreNPToEnvironment opcode.
pub fn handle_store_np_to_environment(inst: &crate::Instruction) -> Option<FlowResult> {
    handle_store_to_environment(inst)
}
