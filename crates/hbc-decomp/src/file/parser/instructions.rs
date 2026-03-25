use crate::error::{Error, Result};
use crate::file::{BytecodeFile, Instruction};
use crate::io::ByteReader;
use crate::opcode::{BytecodeFormat, Operand};

pub fn decode_function_instructions(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
) -> Result<Vec<Instruction>> {
    let header = file
        .function_headers
        .get(function_id as usize)
        .ok_or_else(|| Error::Parse(format!("invalid function id: {function_id}")))?;

    let func_offset = header
        .offset()
        .checked_sub(file.instruction_offset)
        .ok_or_else(|| Error::Parse("function offset underflow".to_string()))?;

    let size = header.bytecode_size_in_bytes() as usize;
    let start = func_offset as usize;
    let end = start + size;
    if end > file.instructions.len() {
        return Err(Error::Parse(format!(
            "function {function_id} bytecode out of range"
        )));
    }

    let mut reader = ByteReader::new(&file.instructions[start..end]);
    let mut offset = 0u32;
    let mut instructions = Vec::new();

    while reader.remaining() > 0 {
        let start_pos = reader.position();
        let opcode = reader.read_u8()?;
        let def = format
            .definitions
            .get(opcode as usize)
            .ok_or_else(|| Error::Parse(format!("unknown opcode: {opcode}")))?;
        let mut operands = Vec::with_capacity(def.operand_types.len());
        for operand_type in &def.operand_types {
            let value = operand_type.read(&mut reader)?;
            operands.push(Operand {
                ty: *operand_type,
                value,
            });
        }
        let end_pos = reader.position();
        let length = (end_pos - start_pos) as u32;
        instructions.push(Instruction {
            offset,
            opcode,
            operands,
            length,
        });
        offset = offset
            .checked_add(length)
            .ok_or_else(|| Error::Parse("instruction offset overflow".to_string()))?;
    }

    Ok(instructions)
}
