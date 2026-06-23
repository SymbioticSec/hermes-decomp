// Opcode handlers for switch statement operations.

use super::opcodes_flow::FlowResult;
use super::opcodes_load::reg_expr;
use crate::ir::{Expression, Statement};
use crate::BytecodeFile;

// Handle SwitchImm opcode.
pub fn handle_switch_imm(
    inst: &crate::Instruction,
    _format: &crate::BytecodeFormat,
    file: &BytecodeFile,
    func_bytecode_offset: u32,
) -> Option<FlowResult> {
    let val = reg_expr(&inst.operands, 0)?;
    // Operands: Reg8 val, UInt32 jmpTableIdx, Addr32 defaultAddr, UInt32 minVal, UInt32 maxVal
    let jmp_table_idx = inst.operands.get(1)?.value.as_u32()?;
    let default_offset = inst.operands.get(2)?.value.as_i32()?;
    let min_val = inst.operands.get(3)?.value.as_u32()?;
    let max_val = inst.operands.get(4)?.value.as_u32()?;

    let default_target = (inst.offset as i32).wrapping_add(default_offset) as u32;

    let table_start_local = (inst.offset as usize).saturating_add(jmp_table_idx as usize);
    let table_start_global = table_start_local.saturating_add(func_bytecode_offset as usize);

    // Malformed bytecode can have maxVal < minVal; a plain `max_val - min_val`
    // underflows (panics in debug, wraps to ~4 billion in release and then
    // blows up Vec::with_capacity). Reject the bad range instead.
    let Some(span) = max_val.checked_sub(min_val) else {
        return Some(FlowResult::Statement(Statement::Comment(format!(
            "SwitchImm: invalid range (maxVal {max_val} < minVal {min_val})"
        ))));
    };
    let count = span as usize + 1;

    // Bounds-check BEFORE allocating so a huge (or corrupt) table can't trigger
    // a capacity-overflow abort in Vec::with_capacity.
    if table_start_global.saturating_add(count.saturating_mul(4)) > file.instructions.len() {
        return Some(FlowResult::Statement(Statement::Comment(format!(
            "SwitchImm: jump table out of bounds (start={}, count={}, len={})",
            table_start_global,
            count,
            file.instructions.len()
        ))));
    }

    let mut cases = Vec::with_capacity(count);

    use crate::io::ByteReader;
    let mut reader = ByteReader::new(&file.instructions[table_start_global..]);

    for i in 0..count {
        if let Ok(rel_offset) = reader.read_i32() {
            let target = (inst.offset as i32).wrapping_add(rel_offset) as u32;
            let case_val = min_val.wrapping_add(i as u32);
            cases.push((
                Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::Integer(case_val as i32))),
                target,
            ));
        }
    }

    Some(FlowResult::Switch {
        value: val,
        default: default_target,
        cases,
    })
}

// Handle StringSwitchImm opcode.
// Operands: Reg8 val, UInt32 jmpTableIdx, UInt32 numCases, Addr32 defaultAddr, UInt32 stringTableOffset
pub fn handle_string_switch_imm(
    inst: &crate::Instruction,
    _format: &crate::BytecodeFormat,
    file: &BytecodeFile,
    func_bytecode_offset: u32,
) -> Option<FlowResult> {
    let val = reg_expr(&inst.operands, 0)?;
    let jmp_table_idx = inst.operands.get(1)?.value.as_u32()?;
    let num_cases = inst.operands.get(2)?.value.as_u32()?;
    let default_offset = inst.operands.get(3)?.value.as_i32()?;
    let string_table_offset = inst.operands.get(4)?.value.as_u32()?;

    let default_target = (inst.offset as i32).wrapping_add(default_offset) as u32;

    let table_start_local = (inst.offset as usize).saturating_add(jmp_table_idx as usize);
    let table_start_global = table_start_local.saturating_add(func_bytecode_offset as usize);

    let count = num_cases as usize;

    // Bounds-check before allocating so a huge numCases can't capacity-overflow.
    if table_start_global.saturating_add(count.saturating_mul(4)) > file.instructions.len() {
        return Some(FlowResult::Statement(Statement::Comment(format!(
            "StringSwitchImm: jump table out of bounds (start={}, count={}, len={})",
            table_start_global,
            count,
            file.instructions.len()
        ))));
    }

    let mut cases = Vec::with_capacity(count);

    use crate::io::ByteReader;
    let mut reader = ByteReader::new(&file.instructions[table_start_global..]);

    for i in 0..count {
        if let Ok(rel_offset) = reader.read_i32() {
            let target = (inst.offset as i32).wrapping_add(rel_offset) as u32;
            let case_str = file
                .string_at(string_table_offset.wrapping_add(i as u32))
                .map(|e| e.value.clone())
                .unwrap_or_else(|| format!("string{}", string_table_offset.wrapping_add(i as u32)));
            cases.push((
                Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::String(case_str))),
                target,
            ));
        }
    }

    Some(FlowResult::Switch {
        value: val,
        default: default_target,
        cases,
    })
}
