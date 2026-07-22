// Jump target analysis for basic block construction.

use crate::file::ExceptionHandler;
use crate::opcode::OperandType;
use crate::{BytecodeFile, BytecodeFormat, Instruction};
use std::collections::BTreeSet;

// Find block starts, including exception handler targets as block boundaries.
pub fn find_block_starts_with_handlers(
    insts: &[Instruction],
    format: &BytecodeFormat,
    file: &BytecodeFile,
    exception_handlers: &[ExceptionHandler],
    func_bytecode_offset: u32,
) -> BTreeSet<u32> {
    let mut targets = BTreeSet::new();

    // First instruction is always a block start
    if let Some(first) = insts.first() {
        targets.insert(first.offset);
    }

    for inst in insts {
        let def = match format.definitions.get(inst.opcode as usize) {
            Some(d) => d,
            None => continue,
        };

        let name = def.name.as_str();

        // Instructions that end a block - next instruction starts a new block
        if matches!(name, "Ret" | "Throw" | "Jmp" | "JmpLong") {
            // The instruction after this one starts a new block (if it exists)
            let next_offset = inst.offset.wrapping_add(inst.length);
            targets.insert(next_offset);
        }

        // Jump instructions - their targets are block starts
        if def.is_jump {
            for operand in &inst.operands {
                if matches!(operand.ty, OperandType::Addr8 | OperandType::Addr32) {
                    if let Some(rel) = operand.value.as_i32() {
                        let target = (inst.offset as i32).wrapping_add(rel);
                        if target >= 0 {
                            targets.insert(target as u32);
                        }
                    }
                }
            }
            // Conditional jumps also have a fall-through to next instruction
            if is_conditional_jump(name) {
                targets.insert(inst.offset.wrapping_add(inst.length));
            }
        }

        // Handle SwitchImm/UIntSwitchImm specially (same operand layout)
        if name == "SwitchImm" || name == "UIntSwitchImm" {
            // Operands: Reg8 val, UInt32 jmpTableIdx, Addr32 defaultAddr, UInt32 minVal, UInt32 maxVal
            if let (Some(jmp_table_op), Some(default_op), Some(min_op), Some(max_op)) = (
                inst.operands.get(1),
                inst.operands.get(2),
                inst.operands.get(3),
                inst.operands.get(4),
            ) {
                if let (Some(jmp_table_idx), Some(default_offset), Some(min_val), Some(max_val)) = (
                    jmp_table_op.value.as_u32(),
                    default_op.value.as_i32(),
                    min_op.value.as_u32(),
                    max_op.value.as_u32(),
                ) {
                    // Default target
                    let default_target = (inst.offset as i32).wrapping_add(default_offset) as u32;
                    targets.insert(default_target);

                    // Read jump table: jmpTableIdx is a byte offset from the SwitchImm instruction
                    let table_start_local = (inst.offset as usize).saturating_add(jmp_table_idx as usize);
                    let table_start_global = table_start_local.saturating_add(func_bytecode_offset as usize);
                    // Guard against maxVal < minVal (would underflow) in malformed bytecode.
                    let count = max_val.checked_sub(min_val).map_or(0, |span| span as usize + 1);

                    if count > 0
                        && table_start_global.saturating_add(count.saturating_mul(4))
                            <= file.instructions.len()
                    {
                        use crate::io::ByteReader;
                        let mut reader = ByteReader::new(&file.instructions[table_start_global..]);
                        for _ in 0..count {
                            if let Ok(rel_offset) = reader.read_i32() {
                                let target = (inst.offset as i32).wrapping_add(rel_offset) as u32;
                                targets.insert(target);
                            }
                        }
                    }
                }
            }
        }

        // Handle StringSwitchImm (different operand layout: numCases instead of min/max)
        if name == "StringSwitchImm" {
            // Operands: Reg8 val, UInt32 jmpTableIdx, UInt32 numCases, Addr32 defaultAddr, UInt32 stringTableOffset
            if let (Some(jmp_table_op), Some(num_cases_op), Some(default_op)) = (
                inst.operands.get(1),
                inst.operands.get(2),
                inst.operands.get(3),
            ) {
                if let (Some(jmp_table_idx), Some(num_cases), Some(default_offset)) = (
                    jmp_table_op.value.as_u32(),
                    num_cases_op.value.as_u32(),
                    default_op.value.as_i32(),
                ) {
                    let default_target = (inst.offset as i32).wrapping_add(default_offset) as u32;
                    targets.insert(default_target);

                    let table_start_local = (inst.offset as usize).saturating_add(jmp_table_idx as usize);
                    let table_start_global = table_start_local.saturating_add(func_bytecode_offset as usize);
                    let count = num_cases as usize;

                    if table_start_global + count * 4 <= file.instructions.len() {
                        use crate::io::ByteReader;
                        let mut reader = ByteReader::new(&file.instructions[table_start_global..]);
                        for _ in 0..count {
                            if let Ok(rel_offset) = reader.read_i32() {
                                let target = (inst.offset as i32).wrapping_add(rel_offset) as u32;
                                targets.insert(target);
                            }
                        }
                    }
                }
            }
        }
    }

    // Add exception handler targets as block starts
    for handler in exception_handlers {
        targets.insert(handler.target);
        // Also mark try region start and end as block boundaries
        targets.insert(handler.start);
        if handler.end > 0 {
            targets.insert(handler.end);
        }
    }

    // Keep only leaders that land on a real instruction. A terminator such as the
    // final Ret records the offset just past itself as a leader, and a try region
    // end is exclusive; when either points at the end of the function it would
    // otherwise create an empty, unreachable phantom block.
    let inst_offsets: BTreeSet<u32> = insts.iter().map(|i| i.offset).collect();
    targets.retain(|t| inst_offsets.contains(t));

    targets
}

// Check if an instruction is a conditional jump.
pub fn is_conditional_jump(name: &str) -> bool {
    matches!(
        name,
        "JmpTrue"
            | "JmpTrueLong"
            | "JmpFalse"
            | "JmpFalseLong"
            | "JEqual"
            | "JNotEqual"
            | "JStrictEqual"
            | "JStrictNotEqual"
            | "JLess"
            | "JLessEqual"
            | "JGreater"
            | "JGreaterEqual"
            | "JLessN"
            | "JLessEqualN"
            | "JGreaterN"
            | "JGreaterEqualN"
            | "JNotLess"
            | "JNotLessEqual"
            | "JNotGreater"
            | "JNotGreaterEqual"
            | "JNotLessN"
            | "JNotLessEqualN"
            | "JNotGreaterN"
            | "JNotGreaterEqualN"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_conditional_jump() {
        assert!(is_conditional_jump("JmpTrue"));
        assert!(is_conditional_jump("JStrictEqual"));
        assert!(!is_conditional_jump("Jmp"));
        assert!(!is_conditional_jump("Ret"));
    }
}
