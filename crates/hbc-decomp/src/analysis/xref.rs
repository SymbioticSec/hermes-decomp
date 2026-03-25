use crate::file::{BytecodeFile, Instruction};
use crate::opcode::{BytecodeFormat, OperandType};

#[derive(Debug)]
pub struct XrefResult {
    pub function_id: u32,
    pub offset: u32,
    pub opcode: String,
}

pub fn find_string_xrefs(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    query: &str,
) -> Vec<XrefResult> {
    let mut results = Vec::new();

    let query_lower = query.to_lowercase();
    let matching_ids: Vec<u32> = (0..file.header.string_count)
        .filter(|&id| {
            if let Some(s) = file.string_at(id) {
                s.value.to_lowercase().contains(&query_lower)
            } else {
                false
            }
        })
        .collect();

    if matching_ids.is_empty() {
        return results;
    }

    for (i, _header) in file.function_headers.iter().enumerate() {
        let function_id = i as u32;
        if let Ok(instructions) = file.decode_function_instructions(format, function_id) {
            for insn in instructions {
                if has_string_operand(&insn, &matching_ids) {
                    let opcode_name = format
                        .definitions
                        .get(insn.opcode as usize)
                        .map(|d| d.name.clone())
                        .unwrap_or_else(|| format!("op{}", insn.opcode));
                    results.push(XrefResult {
                        function_id,
                        offset: insn.offset,
                        opcode: opcode_name,
                    });
                }
            }
        }
    }

    results
}

pub fn find_function_refs(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    target_func_id: u32,
) -> Vec<XrefResult> {
    let mut results = Vec::new();

    for (i, _header) in file.function_headers.iter().enumerate() {
        let function_id = i as u32;
        if let Ok(instructions) = file.decode_function_instructions(format, function_id) {
            for insn in instructions {
                if has_function_operand(&insn, target_func_id) {
                    let opcode_name = format
                        .definitions
                        .get(insn.opcode as usize)
                        .map(|d| d.name.clone())
                        .unwrap_or_else(|| format!("op{}", insn.opcode));
                    results.push(XrefResult {
                        function_id,
                        offset: insn.offset,
                        opcode: opcode_name,
                    });
                }
            }
        }
    }

    results
}

fn has_string_operand(insn: &Instruction, ids: &[u32]) -> bool {
    for operand in &insn.operands {
        match operand.ty {
            OperandType::UInt8S | OperandType::UInt16S | OperandType::UInt32S => {
                if let Some(val) = operand.value.as_u32() {
                    if ids.contains(&val) {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

fn has_function_operand(insn: &Instruction, func_id: u32) -> bool {
    for operand in &insn.operands {
        match operand.ty {
            OperandType::UInt16 | OperandType::UInt32 => {
                // Heuristic: Check opcodes that take function IDs (CreateClosure, etc)
                // Ideally we check instruction definition but for now we check value match
                // and assume calling code filters by opcode or context.
                // Actually this is risky for registers.
                // We should strictly check index operands for FunctionID context.
                // But BytecodeFormat doesn't explicitly type FunctionID vs other UInt.
                // We rely on numeric match for now, user manually verifies.
                if let Some(val) = operand.value.as_u32() {
                    if val == func_id {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}
