// Opcode handlers for environment/closure operations.

use super::env_state::EnvRegMap;
use super::opcodes_flow::FlowResult;
use super::opcodes_load::{get_reg, reg_expr};
use crate::ir::{Expression, Statement};

// CreateEnvironment / CreateFunctionEnvironment / CreateTopLevelEnvironment /
// CreateInnerEnvironment, result register holds the *current* function env
// (nesting level 0).
pub fn handle_create_environment(
    inst: &crate::Instruction,
    env_map: &mut EnvRegMap,
) -> Option<FlowResult> {
    let dst = get_reg(&inst.operands, 0)?;
    env_map.set_level(dst, 0);
    // No visible JS statement, pure env setup.
    Some(FlowResult::Noop)
}

// GetEnvironment rDst, level  (classic: 2 operands)
// GetEnvironment rDst, rEnv, level  (some modern tables: 3 operands)
// GetParentEnvironment rDst, level, same idea (level relative to current).
pub fn handle_get_environment(
    inst: &crate::Instruction,
    env_map: &mut EnvRegMap,
) -> Option<FlowResult> {
    let dst = get_reg(&inst.operands, 0)?;
    // Level is the last integer operand (classic: op1; modern GetEnv: op2).
    let level = inst
        .operands
        .iter()
        .rev()
        .find_map(|op| op.value.as_u32())
        .unwrap_or(0);
    env_map.set_level(dst, level);
    Some(FlowResult::Noop)
}

// GetClosureEnvironment rDst, rClosure, env of a closure value.
// Without resolving the closure object we cannot know the absolute level; leave
// unknown (level_of defaults to 0). Still a no-op for IR statements.
pub fn handle_get_closure_environment(
    inst: &crate::Instruction,
    _env_map: &mut EnvRegMap,
) -> Option<FlowResult> {
    let _dst = get_reg(&inst.operands, 0)?;
    Some(FlowResult::Noop)
}

// LoadFromEnvironment rDst, rEnv, slot
pub fn handle_load_from_environment(
    inst: &crate::Instruction,
    env_map: &EnvRegMap,
) -> Option<FlowResult> {
    let dst = get_reg(&inst.operands, 0)?;
    let env_reg = get_reg(&inst.operands, 1)?;
    let slot = inst.operands.get(2)?.value.as_u32()?;
    let level = env_map.level_of(env_reg);

    Some(FlowResult::Statement(Statement::Assign {
        target: crate::ir::AssignTarget::Register(dst),
        value: Expression::Value(crate::ir::Value::ClosureVar { level, slot }),
    }))
}

// StoreToEnvironment rEnv, slot, rValue
pub fn handle_store_to_environment(
    inst: &crate::Instruction,
    env_map: &EnvRegMap,
) -> Option<FlowResult> {
    let env_reg = get_reg(&inst.operands, 0)?;
    let slot = inst.operands.get(1)?.value.as_u32()?;
    let value = reg_expr(&inst.operands, 2)?;
    let level = env_map.level_of(env_reg);

    Some(FlowResult::Statement(Statement::Assign {
        target: crate::ir::AssignTarget::ClosureVar { level, slot },
        value,
    }))
}

pub fn handle_store_np_to_environment(
    inst: &crate::Instruction,
    env_map: &EnvRegMap,
) -> Option<FlowResult> {
    handle_store_to_environment(inst, env_map)
}
