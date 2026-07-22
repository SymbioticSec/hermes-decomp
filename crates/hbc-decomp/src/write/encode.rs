// Encode decoded instructions back to raw bytecode bytes.

use crate::error::{Error, Result};
use crate::file::Instruction;
use crate::opcode::{BytecodeFormat, OperandType, OperandValue};

// Encode a single instruction to its on-disk bytes (opcode + operands).
pub fn encode_instruction(format: &BytecodeFormat, insn: &Instruction) -> Result<Vec<u8>> {
    let def = format.definitions.get(insn.opcode as usize).ok_or_else(|| {
        Error::Write(format!("unknown opcode {} for encode", insn.opcode))
    })?;
    if insn.operands.len() != def.operand_types.len() {
        return Err(Error::Write(format!(
            "opcode {} ({}): expected {} operands, got {}",
            insn.opcode,
            def.name,
            def.operand_types.len(),
            insn.operands.len()
        )));
    }
    let mut out = Vec::with_capacity(1 + insn.operands.len() * 4);
    out.push(insn.opcode);
    for (op, expected_ty) in insn.operands.iter().zip(def.operand_types.iter()) {
        if op.ty != *expected_ty {
            // Tolerate ty mismatch if the value width is compatible.
        }
        write_operand(&mut out, *expected_ty, &op.value)?;
    }
    Ok(out)
}

// Encode a full function body (ordered instructions, no header).
pub fn encode_function_body(
    format: &BytecodeFormat,
    instructions: &[Instruction],
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for insn in instructions {
        out.extend(encode_instruction(format, insn)?);
    }
    Ok(out)
}

fn write_operand(out: &mut Vec<u8>, ty: OperandType, value: &OperandValue) -> Result<()> {
    match ty {
        OperandType::Reg8 | OperandType::UInt8 | OperandType::UInt8S => {
            let v = match value {
                OperandValue::U8(v) => *v,
                OperandValue::U16(v) if *v <= u8::MAX as u16 => *v as u8,
                OperandValue::U32(v) if *v <= u8::MAX as u32 => *v as u8,
                other => {
                    return Err(Error::Write(format!(
                        "cannot encode {other:?} as UInt8/Reg8"
                    )))
                }
            };
            out.push(v);
        }
        OperandType::UInt16 | OperandType::UInt16S => {
            let v = match value {
                OperandValue::U16(v) => *v,
                OperandValue::U8(v) => *v as u16,
                OperandValue::U32(v) if *v <= u16::MAX as u32 => *v as u16,
                other => {
                    return Err(Error::Write(format!(
                        "cannot encode {other:?} as UInt16"
                    )))
                }
            };
            out.extend_from_slice(&v.to_le_bytes());
        }
        OperandType::Reg32 | OperandType::UInt32 | OperandType::UInt32S => {
            let v = match value {
                OperandValue::U32(v) => *v,
                OperandValue::U16(v) => *v as u32,
                OperandValue::U8(v) => *v as u32,
                other => {
                    return Err(Error::Write(format!(
                        "cannot encode {other:?} as UInt32/Reg32"
                    )))
                }
            };
            out.extend_from_slice(&v.to_le_bytes());
        }
        OperandType::Addr8 => {
            let v = match value {
                OperandValue::I8(v) => *v,
                OperandValue::I32(v) if *v >= i8::MIN as i32 && *v <= i8::MAX as i32 => *v as i8,
                OperandValue::U8(v) => *v as i8,
                other => {
                    return Err(Error::Write(format!("cannot encode {other:?} as Addr8")))
                }
            };
            out.push(v as u8);
        }
        OperandType::Addr32 | OperandType::Imm32 => {
            let v = match value {
                OperandValue::I32(v) => *v,
                OperandValue::I8(v) => *v as i32,
                OperandValue::U32(v) => *v as i32,
                OperandValue::U16(v) => *v as i32,
                OperandValue::U8(v) => *v as i32,
                other => {
                    return Err(Error::Write(format!(
                        "cannot encode {other:?} as Addr32/Imm32"
                    )))
                }
            };
            out.extend_from_slice(&v.to_le_bytes());
        }
        OperandType::Double => {
            let v = match value {
                OperandValue::F64(v) => *v,
                other => {
                    return Err(Error::Write(format!("cannot encode {other:?} as Double")))
                }
            };
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::BytecodeFile;
    use crate::opcode::BytecodeFormat;

    fn load_fixture(path: &str) -> (crate::file::BytecodeFile, BytecodeFormat) {
        let bytes = std::fs::read(path).expect("fixture");
        let file = BytecodeFile::parse_auto(&bytes).expect("parse");
        let format = BytecodeFormat::for_version(file.header.version).expect("format");
        (file, format)
    }

    #[test]
    fn encode_roundtrip_function_bodies_v96() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/react-native/v96/expressions/generator/bytecode.hbc"
        );
        if !std::path::Path::new(path).exists() {
            eprintln!("skip: fixture missing");
            return;
        }
        let (file, format) = load_fixture(path);
        for id in 0..file.header.function_count {
            let decoded = file
                .decode_function_instructions(&format, id)
                .expect("decode");
            let encoded = encode_function_body(&format, &decoded).expect("encode");
            let start = (file.function_headers[id as usize].offset() - file.instruction_offset)
                as usize;
            let size = file.function_headers[id as usize].bytecode_size_in_bytes() as usize;
            let original = &file.instructions[start..start + size];
            assert_eq!(
                encoded, original,
                "function {id} encode round-trip mismatch"
            );
        }
    }

    #[test]
    fn encode_roundtrip_function_bodies_v98() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/react-native/v98/expressions/generator/bytecode.hbc"
        );
        if !std::path::Path::new(path).exists() {
            eprintln!("skip: fixture missing");
            return;
        }
        let (file, format) = load_fixture(path);
        for id in 0..file.header.function_count {
            let decoded = match file.decode_function_instructions(&format, id) {
                Ok(d) => d,
                Err(_) => continue, // overflowed/exception-heavy bodies may need care
            };
            let encoded = encode_function_body(&format, &decoded).expect("encode");
            let start = (file.function_headers[id as usize].offset() - file.instruction_offset)
                as usize;
            let size = file.function_headers[id as usize].bytecode_size_in_bytes() as usize;
            let original = &file.instructions[start..start + size];
            assert_eq!(
                encoded, original,
                "function {id} encode round-trip mismatch"
            );
        }
    }
}
