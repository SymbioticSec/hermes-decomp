// Parse HASM text (our disasm dialect) back into instructions.

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::file::{BytecodeFile, Instruction};
use crate::opcode::{BytecodeFormat, Operand, OperandType, OperandValue};
use crate::util::escape_js_string;

use super::{HasmFunction, HasmModule};

// Parse a single function body in our disasm dialect into instructions.
//
// Accepts lines like:
//   0000  LoadConstUndefined r0
//   LoadConstString r1, "hello"
//   JmpTrue L12, r0
//   L12:
//   Ret r0
//
// `string_lookup` maps literal content → string id (from the host BytecodeFile).
pub fn parse_hasm_function(
    text: &str,
    format: &BytecodeFormat,
    string_lookup: &HashMap<String, u32>,
) -> Result<Vec<Instruction>> {
    // Opcode name → opcode byte (first definition wins; names are unique per version).
    let mut name_to_op: HashMap<String, u8> = HashMap::new();
    for def in &format.definitions {
        if def.name != "<invalid>" {
            name_to_op.entry(def.name.clone()).or_insert(def.opcode);
        }
    }

    // Pass 1: collect labels and raw lines (mnemonic + operand tokens).
    struct RawLine {
        label_here: Option<String>,
        mnemonic: Option<String>,
        ops: Vec<String>,
    }
    let mut raw_lines: Vec<RawLine> = Vec::new();
    let mut pending_label: Option<String> = None;

    for line in text.lines() {
        let line = strip_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        // Skip function banners
        if line.starts_with("function ") || line.starts_with("; ") || line.starts_with("Function") {
            continue;
        }
        // Label-only line: L123:
        if let Some(lab) = line.strip_suffix(':') {
            if lab.starts_with('L') && lab[1..].chars().all(|c| c.is_ascii_digit()) {
                pending_label = Some(lab.to_string());
                continue;
            }
        }

        let mut rest = line;
        // Optional hex offset prefix "0000  " or "0x0000 "
        if let Some(after) = strip_offset_prefix(rest) {
            rest = after;
        }

        let mut parts = split_mnemonic_operands(rest);
        if parts.is_empty() {
            continue;
        }
        let mnemonic = parts.remove(0);
        raw_lines.push(RawLine {
            label_here: pending_label.take(),
            mnemonic: Some(mnemonic),
            ops: parts,
        });
    }

    // Pass 2: compute instruction sizes and label → offset map.
    let mut label_offsets: HashMap<String, u32> = HashMap::new();
    let mut offset = 0u32;
    let mut sized2: Vec<(u32, u8, Vec<String>, Vec<OperandType>)> = Vec::new();

    for raw in &raw_lines {
        if let Some(lab) = &raw.label_here {
            label_offsets.insert(lab.clone(), offset);
        }
        let mnem = raw.mnemonic.as_ref().unwrap();
        let op = *name_to_op
            .get(mnem)
            .ok_or_else(|| Error::Write(format!("unknown mnemonic: {mnem}")))?;
        let def = &format.definitions[op as usize];
        if raw.ops.len() != def.operand_types.len() {
            return Err(Error::Write(format!(
                "{mnem}: expected {} operands, got {} ({:?})",
                def.operand_types.len(),
                raw.ops.len(),
                raw.ops
            )));
        }
        let len = 1 + def
            .operand_types
            .iter()
            .map(|t| operand_size(*t))
            .sum::<usize>() as u32;
        sized2.push((offset, op, raw.ops.clone(), def.operand_types.clone()));
        offset = offset.wrapping_add(len);
    }

    // Pass 3: build Instructions with resolved jumps/strings.
    let mut out = Vec::with_capacity(sized2.len());
    for (off, opcode, ops, types) in sized2 {
        let mut operands = Vec::with_capacity(types.len());
        for (tok, ty) in ops.iter().zip(types.iter()) {
            let value = parse_operand_token(tok, *ty, off, &label_offsets, string_lookup)?;
            operands.push(Operand { ty: *ty, value });
        }
        let length = 1 + types.iter().map(|t| operand_size(*t)).sum::<usize>() as u32;
        out.push(Instruction {
            offset: off,
            opcode,
            operands,
            length,
        });
    }
    Ok(out)
}

// Parse multi-function disasm text (best-effort). Functions start with
// `function N:` or `; fn#N`.
pub fn parse_hasm(text: &str) -> Result<HasmModule> {
    // Without format/string table we only split function text; full parse needs
    // parse_hasm_with_context.
    let mut functions = Vec::new();
    let mut current_id = 0u32;
    let mut current_name = None;
    let mut buf = String::new();

    let flush = |id: u32,
                 name: &Option<String>,
                 buf: &str,
                 functions: &mut Vec<HasmFunction>| {
        if buf.trim().is_empty() {
            return;
        }
        functions.push(HasmFunction {
            id,
            name: name.clone(),
            instructions: Vec::new(), // filled by parse_hasm_with_context
            exception_handlers: Vec::new(),
        });
        // store raw text in name field temporarily? keep empty body
        let _ = buf;
    };

    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("function ") {
            if !buf.is_empty() {
                functions.push(HasmFunction {
                    id: current_id,
                    name: current_name.take(),
                    instructions: Vec::new(),
                    exception_handlers: Vec::new(),
                });
                buf.clear();
            }
            // function 3: or function foo (bar):
            if let Some(id_str) = rest.split(':').next() {
                if let Ok(id) = id_str.trim().parse::<u32>() {
                    current_id = id;
                }
            }
            continue;
        }
        if t.starts_with("; fn#") {
            if !buf.is_empty() {
                functions.push(HasmFunction {
                    id: current_id,
                    name: current_name.take(),
                    instructions: Vec::new(),
                    exception_handlers: Vec::new(),
                });
                buf.clear();
            }
            // ; fn#2 "gen" ...
            if let Some(num) = t.strip_prefix("; fn#") {
                let id_part = num.split_whitespace().next().unwrap_or("0");
                current_id = id_part.parse().unwrap_or(0);
                if let Some(q) = t.find('"') {
                    let rest = &t[q + 1..];
                    if let Some(end) = rest.find('"') {
                        current_name = Some(rest[..end].to_string());
                    }
                }
            }
            continue;
        }
        buf.push_str(line);
        buf.push('\n');
    }
    if !buf.is_empty() {
        functions.push(HasmFunction {
            id: current_id,
            name: current_name,
            instructions: Vec::new(),
            exception_handlers: Vec::new(),
        });
    }
    let _ = flush;
    Ok(HasmModule {
        version: None,
        functions,
    })
}

// Parse function body text with full context (format + host file for strings).
pub fn parse_hasm_with_context(
    text: &str,
    format: &BytecodeFormat,
    file: &BytecodeFile,
) -> Result<Vec<Instruction>> {
    let mut lookup = HashMap::new();
    for (i, s) in file.strings.iter().enumerate() {
        lookup.entry(s.value.clone()).or_insert(i as u32);
    }
    parse_hasm_function(text, format, &lookup)
}

fn operand_size(ty: OperandType) -> usize {
    match ty {
        OperandType::Reg8 | OperandType::UInt8 | OperandType::UInt8S | OperandType::Addr8 => 1,
        OperandType::UInt16 | OperandType::UInt16S => 2,
        OperandType::Reg32
        | OperandType::UInt32
        | OperandType::UInt32S
        | OperandType::Addr32
        | OperandType::Imm32 => 4,
        OperandType::Double => 8,
    }
}

fn strip_comment(line: &str) -> &str {
    // Only strip `//` comments outside of quotes (simple scan).
    let mut in_str = false;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let c = bytes[i];
        if c == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
            in_str = !in_str;
        }
        if !in_str && c == b'/' && bytes[i + 1] == b'/' {
            return &line[..i];
        }
        i += 1;
    }
    line
}

fn strip_offset_prefix(line: &str) -> Option<&str> {
    let t = line.trim_start();
    // 0000  Mnemonic  or  0x0000
    let mut chars = t.char_indices();
    let mut hex_end = 0;
    let mut saw_hex = false;
    if t.starts_with("0x") || t.starts_with("0X") {
        hex_end = 2;
        saw_hex = true;
        for (i, c) in t[2..].char_indices() {
            if c.is_ascii_hexdigit() {
                hex_end = 2 + i + 1;
            } else {
                break;
            }
        }
    } else {
        for (i, c) in chars.by_ref() {
            if c.is_ascii_hexdigit() {
                hex_end = i + 1;
                saw_hex = true;
            } else {
                break;
            }
        }
    }
    if !saw_hex || hex_end == 0 {
        return None;
    }
    let rest = t[hex_end..].trim_start();
    // Must look like a mnemonic after offset
    if rest
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic())
        .unwrap_or(false)
    {
        Some(rest)
    } else {
        None
    }
}

fn split_mnemonic_operands(line: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    let mut chars = line.chars().peekable();
    // First token: mnemonic
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            break;
        }
        cur.push(c);
        chars.next();
    }
    if !cur.is_empty() {
        parts.push(cur);
        cur = String::new();
    }
    // Skip whitespace
    while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
        chars.next();
    }
    // Remaining: comma-separated operands
    for c in chars {
        if c == '"' {
            in_str = !in_str;
            cur.push(c);
            continue;
        }
        if c == ',' && !in_str {
            parts.push(cur.trim().to_string());
            cur.clear();
            continue;
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        parts.push(cur.trim().to_string());
    }
    parts
}

fn parse_operand_token(
    tok: &str,
    ty: OperandType,
    insn_offset: u32,
    labels: &HashMap<String, u32>,
    strings: &HashMap<String, u32>,
) -> Result<OperandValue> {
    let tok = tok.trim();
    match ty {
        OperandType::Reg8 | OperandType::Reg32 => {
            let n = tok
                .strip_prefix('r')
                .or_else(|| tok.strip_prefix('R'))
                .ok_or_else(|| Error::Write(format!("expected register, got {tok}")))?;
            let v: u32 = n
                .parse()
                .map_err(|_| Error::Write(format!("bad register {tok}")))?;
            if matches!(ty, OperandType::Reg8) {
                Ok(OperandValue::U8(v as u8))
            } else {
                Ok(OperandValue::U32(v))
            }
        }
        OperandType::Addr8 | OperandType::Addr32 => {
            let target = if let Some(lab) = tok.strip_prefix('L') {
                if let Ok(abs) = lab.parse::<u32>() {
                    abs
                } else {
                    *labels
                        .get(tok)
                        .ok_or_else(|| Error::Write(format!("unknown label {tok}")))?
                }
            } else if let Some(&abs) = labels.get(tok) {
                abs
            } else {
                return Err(Error::Write(format!("bad address operand {tok}")));
            };
            let rel = target as i32 - insn_offset as i32;
            if matches!(ty, OperandType::Addr8) {
                if rel < i8::MIN as i32 || rel > i8::MAX as i32 {
                    return Err(Error::Write(format!("Addr8 out of range: {rel}")));
                }
                Ok(OperandValue::I8(rel as i8))
            } else {
                Ok(OperandValue::I32(rel))
            }
        }
        OperandType::UInt8S | OperandType::UInt16S | OperandType::UInt32S => {
            // String literal or numeric id
            if tok.starts_with('"') {
                let unquoted = unquote(tok)?;
                let id = strings.get(&unquoted).copied().ok_or_else(|| {
                    Error::Write(format!("string not in table: {}", escape_js_string(&unquoted)))
                })?;
                return match ty {
                    OperandType::UInt8S => Ok(OperandValue::U8(id as u8)),
                    OperandType::UInt16S => Ok(OperandValue::U16(id as u16)),
                    _ => Ok(OperandValue::U32(id)),
                };
            }
            parse_uint(tok, ty)
        }
        OperandType::UInt8 | OperandType::UInt16 | OperandType::UInt32 => parse_uint(tok, ty),
        OperandType::Imm32 => {
            let v: i32 = tok
                .parse()
                .map_err(|_| Error::Write(format!("bad imm {tok}")))?;
            Ok(OperandValue::I32(v))
        }
        OperandType::Double => {
            let v: f64 = tok
                .parse()
                .map_err(|_| Error::Write(format!("bad double {tok}")))?;
            Ok(OperandValue::F64(v))
        }
    }
}

fn parse_uint(tok: &str, ty: OperandType) -> Result<OperandValue> {
    let v: u32 = if let Some(h) = tok.strip_prefix("0x").or_else(|| tok.strip_prefix("0X")) {
        u32::from_str_radix(h, 16).map_err(|_| Error::Write(format!("bad hex {tok}")))?
    } else {
        tok.parse()
            .map_err(|_| Error::Write(format!("bad uint {tok}")))?
    };
    Ok(match ty {
        OperandType::UInt8 | OperandType::UInt8S => OperandValue::U8(v as u8),
        OperandType::UInt16 | OperandType::UInt16S => OperandValue::U16(v as u16),
        _ => OperandValue::U32(v),
    })
}

fn unquote(tok: &str) -> Result<String> {
    let t = tok.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        // Minimal unescape
        let inner = &t[1..t.len() - 1];
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    Some(o) => out.push(o),
                    None => break,
                }
            } else {
                out.push(c);
            }
        }
        Ok(out)
    } else {
        Err(Error::Write(format!("expected quoted string, got {tok}")))
    }
}
