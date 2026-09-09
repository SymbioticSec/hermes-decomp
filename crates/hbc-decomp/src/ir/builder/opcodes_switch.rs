// Opcode handlers for switch statement operations.

use super::opcodes_flow::FlowResult;
use super::opcodes_load::reg_expr;
use crate::ir::{Expression, Statement};
use crate::BytecodeFile;

// Hermes stores a switch jump table immediately after the function's bytecode,
// aligned to 4 bytes relative to the function start. The interpreter reads it at
// `align4((ip - functionStart) + jmpTableOffset)`, so the raw `inst.offset +
// jmpTableIdx` must be rounded up to the next multiple of 4 before use; skipping
// this reads the table 1-3 bytes early and yields garbage targets.
fn align4(x: usize) -> usize {
    (x + 3) & !3
}

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

    let table_start_unaligned = (inst.offset as usize)
        .saturating_add(jmp_table_idx as usize)
        .saturating_add(func_bytecode_offset as usize);
    let table_start_global = align4(table_start_unaligned);

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
//
// This is a string-switch variant present in the Hermes build React Native /
// Discord ships (v98+); upstream Hermes has only the numeric `SwitchImm`, so the
// operand semantics and table layout below were recovered from the bytecode
// itself, verified against the real notification-type dispatch it encodes.
//
// Operands: Reg8 val, UInt32 <unused>, UInt32 jmpTableOffset, Addr32 defaultAddr, UInt32 numCases
// The jump table sits at `align4(ip + jmpTableOffset)` and holds `numCases`
// entries of 8 bytes each: a u32 string id followed by an i32 offset relative to
// the instruction. (Operand 1 is some interpreter-side hint and is not needed
// here.) The earlier reading took operand 1 as the table offset and operand 2 as
// the case count, which pointed into the middle of the code and yielded a garbage
// count in the hundreds, burying the real control flow.
pub fn handle_string_switch_imm(
    inst: &crate::Instruction,
    _format: &crate::BytecodeFormat,
    file: &BytecodeFile,
    func_bytecode_offset: u32,
) -> Option<FlowResult> {
    let val = reg_expr(&inst.operands, 0)?;
    let jmp_table_idx = inst.operands.get(2)?.value.as_u32()?;
    let default_offset = inst.operands.get(3)?.value.as_i32()?;
    let num_cases = inst.operands.get(4)?.value.as_u32()?;

    let default_target = (inst.offset as i32).wrapping_add(default_offset) as u32;

    let table_start_unaligned = (inst.offset as usize)
        .saturating_add(jmp_table_idx as usize)
        .saturating_add(func_bytecode_offset as usize);
    let table_start_global = align4(table_start_unaligned);

    let count = num_cases as usize;

    // Bounds-check before allocating so a corrupt numCases can't capacity-overflow.
    // Each entry is 8 bytes (u32 string id + i32 offset).
    if table_start_global.saturating_add(count.saturating_mul(8)) > file.instructions.len() {
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

    for _ in 0..count {
        let (Ok(string_id), Ok(rel_offset)) = (reader.read_u32(), reader.read_i32()) else {
            break;
        };
        let target = (inst.offset as i32).wrapping_add(rel_offset) as u32;
        let case_str = file
            .string_at(string_id)
            .map(|e| e.value.clone())
            .unwrap_or_else(|| format!("string{string_id}"));
        cases.push((
            Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::String(case_str))),
            target,
        ));
    }

    Some(FlowResult::Switch {
        value: val,
        default: default_target,
        cases,
    })
}
