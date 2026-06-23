use std::collections::BTreeSet;

use colored::*;

use crate::error::Result;
use crate::file::{BytecodeFile, Instruction};
use crate::opcode::{BytecodeFormat, Operand, OperandType, OperandValue};
use crate::util::escape_js_string;

#[derive(Debug, Clone)]
pub struct DisasmOptions {
    pub show_offsets: bool,
    pub show_labels: bool,
    pub resolve_strings: bool,
    pub enable_color: bool,
}

impl Default for DisasmOptions {
    fn default() -> Self {
        Self {
            show_offsets: true,
            show_labels: true,
            resolve_strings: true,
            enable_color: false,
        }
    }
}

pub fn disassemble_function(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    options: &DisasmOptions,
) -> Result<String> {
    let instructions = file.decode_function_instructions(format, function_id)?;
    let label_offsets = collect_label_offsets(&instructions, format);
    let mut out = String::new();

    for insn in &instructions {
        if options.show_labels && label_offsets.contains(&insn.offset) {
            let label = format!("L{}:", insn.offset);
            if options.enable_color {
                out.push_str(&format!("{}\n", label.yellow().bold()));
            } else {
                out.push_str(&format!("{label}\n"));
            }
        }
        out.push_str(&format_instruction(insn, file, format, options));
        out.push('\n');
    }

    Ok(out)
}

pub fn disassemble_all(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    options: &DisasmOptions,
) -> Result<String> {
    let mut out = String::new();
    for header in &file.function_headers {
        let function_id = header.function_id();
        let name = file
            .string_at(header.function_name())
            .map(|entry| entry.value.as_str())
            .unwrap_or("");

        let header_str = if name.is_empty() {
            format!("function {function_id}:")
        } else {
            format!("function {} ({}):", function_id, escape_js_string(name))
        };

        if options.enable_color {
            out.push_str(&format!("{}\n", header_str.cyan().bold()));
        } else {
            out.push_str(&format!("{header_str}\n"));
        }

        let body = disassemble_function(file, format, function_id, options)?;
        for line in body.lines() {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    Ok(out)
}

pub fn collect_label_offsets(
    instructions: &[Instruction],
    format: &BytecodeFormat,
) -> BTreeSet<u32> {
    let mut labels = BTreeSet::new();
    for insn in instructions {
        let def = match format.definitions.get(insn.opcode as usize) {
            Some(def) => def,
            None => continue,
        };
        if def.name == "Ret" || def.name == "Throw" || def.name == "Jmp" || def.name == "JmpLong" {
            labels.insert(insn.offset.wrapping_add(insn.length));
        }
        if def.is_jump {
            for operand in &insn.operands {
                if operand.ty == OperandType::Addr8 || operand.ty == OperandType::Addr32 {
                    if let Some(rel) = operand.value.as_i32() {
                        let target = (insn.offset as i32).wrapping_add(rel);
                        if target >= 0 {
                            labels.insert(target as u32);
                        }
                    }
                }
            }
        }
    }
    labels
}

fn format_instruction(
    insn: &Instruction,
    file: &BytecodeFile,
    format: &BytecodeFormat,
    options: &DisasmOptions,
) -> String {
    let def = match format.definitions.get(insn.opcode as usize) {
        Some(def) => def,
        None => {
            return format!("<unknown opcode {}>", insn.opcode);
        }
    };

    let mut line = String::new();
    if options.show_offsets {
        let offset_str = format!("{:04x}  ", insn.offset);
        if options.enable_color {
            line.push_str(&offset_str.white().dimmed().to_string());
        } else {
            line.push_str(&offset_str);
        }
    }

    if options.enable_color {
        line.push_str(&def.name.blue().to_string());
    } else {
        line.push_str(&def.name);
    }

    if !insn.operands.is_empty() {
        line.push(' ');
        let rendered: Vec<String> = insn
            .operands
            .iter()
            .map(|operand| format_operand(insn, operand, file, options))
            .collect();
        line.push_str(&rendered.join(", "));
    }

    line
}

fn format_operand(
    insn: &Instruction,
    operand: &Operand,
    file: &BytecodeFile,
    options: &DisasmOptions,
) -> String {
    match operand.ty {
        OperandType::Reg8 | OperandType::Reg32 => {
            let s = match operand.value {
                OperandValue::U8(v) => format!("r{v}"),
                OperandValue::U16(v) => format!("r{v}"),
                OperandValue::U32(v) => format!("r{v}"),
                _ => "r?".to_string(),
            };
            if options.enable_color {
                s.red().to_string()
            } else {
                s
            }
        }
        OperandType::Addr8 | OperandType::Addr32 => {
            let s = match operand.value.as_i32() {
                Some(rel) => {
                    let target = (insn.offset as i32).wrapping_add(rel);
                    format!("L{target}")
                }
                None => "L?".to_string(),
            };
            if options.enable_color {
                s.yellow().to_string()
            } else {
                s
            }
        }
        OperandType::UInt8S | OperandType::UInt16S | OperandType::UInt32S => {
            if options.resolve_strings {
                if let Some(id) = operand.value.as_u32() {
                    if let Some(entry) = file.string_at(id) {
                        let quoted = escape_js_string(&entry.value);
                        if options.enable_color {
                            return quoted.green().to_string();
                        } else {
                            return quoted;
                        }
                    }
                }
            }
            let s = format_operand_value(&operand.value);
            if options.enable_color {
                s.purple().to_string()
            } else {
                s
            }
        }
        _ => {
            let s = format_operand_value(&operand.value);
            if options.enable_color {
                s.purple().to_string()
            } else {
                s
            }
        }
    }
}

fn format_operand_value(value: &OperandValue) -> String {
    match value {
        OperandValue::U8(v) => v.to_string(),
        OperandValue::U16(v) => v.to_string(),
        OperandValue::U32(v) => v.to_string(),
        OperandValue::I8(v) => v.to_string(),
        OperandValue::I32(v) => v.to_string(),
        OperandValue::F64(v) => {
            if v.is_nan() {
                "NaN".to_string()
            } else if v.is_infinite() {
                if v.is_sign_negative() {
                    "-Infinity".to_string()
                } else {
                    "Infinity".to_string()
                }
            } else {
                v.to_string()
            }
        }
    }
}
