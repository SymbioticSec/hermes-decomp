// Opcode handlers for environment/closure operations.

use super::env_state::EnvRegMap;
use super::opcodes_flow::FlowResult;
use super::opcodes_load::{get_reg, reg_expr};
use crate::ir::{Expression, Statement};

// CreateEnvironment / CreateFunctionEnvironment / CreateTopLevelEnvironment /
// CreateInnerEnvironment, result register holds the *current* function env
// (nesting level 0).
pub fn handle_create_environment(
    name: &str,
    inst: &crate::Instruction,
    env_map: &mut EnvRegMap,
) -> Option<FlowResult> {
    let dst = get_reg(&inst.operands, 0)?;
    if name == "CreateFunctionEnvironment" {
        // First one is the environment the function runs in; any later one is a
        // separate scope for a closure and must not share this function's slots.
        env_map.claim_function_env(dst);
        return Some(FlowResult::Noop);
    }
    env_map.set_level(dst, 0);
    // CreateTopLevelEnvironment / CreateInnerEnvironment / 3-operand
    // CreateEnvironment build an ADDITIONAL environment, a separate scope. It is
    // captured a moment later by `StoreToEnvironment parent, K, thisEnv`; the
    // store gives it the identity of parent slot K so its own slot accesses use
    // the same level the capturing child computes, instead of colliding with the
    // running env's slot names (`email = undefined` over the real login email).
    let creates_new_env = match name {
        "CreateFunctionEnvironment" => false,
        "CreateEnvironment" => inst.operands.len() >= 3,
        "CreateTopLevelEnvironment" | "CreateInnerEnvironment" => true,
        _ => false,
    };
    if creates_new_env {
        env_map.mark_created_env(dst);
    }
    // No visible JS statement, pure env setup.
    Some(FlowResult::Noop)
}

// GetEnvironment rDst, level  (classic: 2 operands)
// GetEnvironment rDst, rEnv, level  (some modern tables: 3 operands)
// GetParentEnvironment rDst, level, same idea (level relative to current).
pub fn handle_get_environment(
    name: &str,
    inst: &crate::Instruction,
    env_map: &mut EnvRegMap,
) -> Option<FlowResult> {
    let dst = get_reg(&inst.operands, 0)?;
    // Level is the last integer operand (classic: op1; modern GetEnv: op2).
    let operand_level = inst
        .operands
        .iter()
        .rev()
        .find_map(|op| op.value.as_u32())
        .unwrap_or(0);
    // `GetParentEnvironment` counts from the environment the function runs in, so
    // it shifts when that environment is not the function's own. `GetEnvironment`
    // takes its base environment as a register operand and is already absolute.
    let level = if name == "GetParentEnvironment" {
        env_map.parent_env_level(operand_level)
    } else {
        operand_level
    };
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
    env_map: &mut EnvRegMap,
) -> Option<FlowResult> {
    let dst = get_reg(&inst.operands, 0)?;
    let env_reg = get_reg(&inst.operands, 1)?;
    let slot = inst.operands.get(2)?.value.as_u32()?;
    let level = env_map.env_level_of(env_reg);
    // Remember where this value came from: if it is used as an environment
    // later, that is what identifies it.
    env_map.set_source_slot(dst, level, slot);

    Some(FlowResult::Statement(Statement::Assign {
        target: crate::ir::AssignTarget::Register(dst),
        value: Expression::Value(crate::ir::Value::ClosureVar { level, slot }),
    }))
}

// StoreToEnvironment rEnv, slot, rValue
pub fn handle_store_to_environment(
    inst: &crate::Instruction,
    env_map: &mut EnvRegMap,
) -> Option<FlowResult> {
    let env_reg = get_reg(&inst.operands, 0)?;
    let slot = inst.operands.get(1)?.value.as_u32()?;
    let level = env_map.env_level_of(env_reg);

    // Capturing a freshly created environment into parent slot `slot`: give it the
    // identity of that slot, so its own slot accesses use the level a load from
    // the same slot produces (see env_level_of). Parent and capturing child then
    // name the shared scope the same way, and its zero-inits stop colliding with
    // the running env's slots.
    if let Some(value_reg) = get_reg(&inst.operands, 2) {
        if env_map.is_created_env(value_reg) {
            env_map.set_source_slot(value_reg, level, slot);
        }
    }

    let value = reg_expr(&inst.operands, 2)?;

    Some(FlowResult::Statement(Statement::Assign {
        target: crate::ir::AssignTarget::ClosureVar { level, slot },
        value,
    }))
}

pub fn handle_store_np_to_environment(
    inst: &crate::Instruction,
    env_map: &mut EnvRegMap,
) -> Option<FlowResult> {
    handle_store_to_environment(inst, env_map)
}
