// Inject a stub into a function body: a runtime no-op pad, or a real
// print(<function name>) prologue for entry logging.

use crate::error::{Error, Result};
use crate::file::{BytecodeFile, Instruction};
use crate::format::FunctionHeader;
use crate::opcode::BytecodeFormat;

use crate::write::encode::encode_function_body;
use crate::write::header_write::read_modern_large_pointer;
use crate::write::serialize::section_offset;

use super::functions::patch_function_body;
use super::PatchOptions;

// Reserve room for the injected `print(name)` call: r0..r3 plus the outgoing call
// frame Hermes lays out above frame_size. A compiled leaf `print("x")` uses about
// frame_size 11, so bump by eight over a four register floor.
fn log_frame_size(current: u32) -> u32 {
    current.max(4) + 8
}

// Modern functions keep frame_size and the read cache count in the out-of-line
// large header (overflowed) or packed in the 12-byte small header (not
// overflowed). Patch them in `raw_bytes` before the resize splice, which copies
// those bytes verbatim, so the runtime allocates the registers the log call needs.
// Returns the read cache slot index to use for the injected TryGetById.
fn reserve_modern_log_regs(file: &mut BytecodeFile, function_id: u32) -> Result<u32> {
    let (frame_now, cache_now) = match file.function_headers.get(function_id as usize) {
        Some(FunctionHeader::Modern(m)) => (m.frame_size, m.read_cache_size as u32),
        _ => return Err(Error::Write("inject log: not a modern function".into())),
    };
    if cache_now + 1 > u8::MAX as u32 {
        return Err(Error::Write("inject log: read cache full".into()));
    }
    let new_frame = log_frame_size(frame_now);
    let fh_sec = section_offset(file, "function_headers")
        .ok_or_else(|| Error::Write("function_headers section missing".into()))?
        as usize;
    let slot = fh_sec + function_id as usize * 12;
    let raw = file
        .raw_bytes
        .as_mut()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;
    // The overflow bit lives in the small header (byte 11). For an overflowed
    // function the parsed struct flags come from the large header instead, which
    // does not carry the bit, so read it straight from the small header here.
    let overflowed = raw[slot + 11] & crate::format::FLAG_OVERFLOWED != 0;
    if overflowed {
        // Large header layout: frame_size is the u32 at +28, read_cache_size the
        // u8 at +32.
        let lp = read_modern_large_pointer(&raw[slot..slot + 12])? as usize;
        if lp + 33 > raw.len() {
            return Err(Error::Write("inject log: large header out of range".into()));
        }
        raw[lp + 28..lp + 32].copy_from_slice(&new_frame.to_le_bytes());
        raw[lp + 32] = (cache_now + 1) as u8;
    } else {
        // Small 12-byte header: frame_size at bits 64..72, read_cache_size at bits
        // 72..80 (the ninth and tenth bytes).
        raw[slot + 8] = (new_frame & 0xff) as u8;
        raw[slot + 9] = (cache_now + 1) as u8;
    }
    // Keep the parsed struct in sync for any later reads in this session.
    if let Some(FunctionHeader::Modern(m)) = file.function_headers.get_mut(function_id as usize) {
        m.frame_size = new_frame;
        m.read_cache_size = (cache_now + 1) as u8;
    }
    Ok(cache_now)
}

// Kinds of bytecode stubs we plan to inject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectStubKind {
    // Minimal no-op: AsyncBreakCheck if available, else leave unchanged.
    NopPad,
    // Prepend LoadConstUndefined; Ret path unchanged, only works if frame has room
    // and we use same-size replace of first instruction with a longer sequence, hard.
    // Instead: append AsyncBreakCheck before Ret when Ret is last and we grow.
    LogEntry,
}

// Build a real `print(<function name>)` prologue and prepend it to `body`.
//
// Uses registers r0..r3 (free at entry, Hermes reads params via LoadParam, not
// pre-loaded frame registers) and one read-cache slot. Bumps `frame_size` and
// `highest_read_cache_index` in the function header so the runtime allocates
// them; those edits persist because the resize path rewrites the full header.
// Legacy, non-overflow, with a `"print"` string in the table.
fn build_log_entry(
    file: &mut BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    body: &mut Vec<Instruction>,
) -> Result<()> {
    use crate::opcode::{Operand, OperandType, OperandValue};

    let opc = |name: &str| {
        format
            .definitions
            .iter()
            .find(|d| d.name == name)
            .map(|d| d.opcode)
    };
    let (Some(op_ggo), Some(op_try), Some(op_lcu), Some(op_lcs), Some(op_call2)) = (
        opc("GetGlobalObject"),
        opc("TryGetById"),
        opc("LoadConstUndefined"),
        opc("LoadConstString"),
        opc("Call2"),
    ) else {
        return Err(Error::Write(
            "inject log: required opcodes unavailable for this bytecode version".into(),
        ));
    };

    let print_id = file
        .strings
        .iter()
        .position(|s| s.value == "print")
        .ok_or_else(|| {
            Error::Write(
                "inject log: no \"print\" string in the table to build a log call".into(),
            )
        })? as u32;

    // Read this function's name string id, then reserve the frame registers and a
    // read cache slot the log call needs. Legacy stores these in the small header
    // struct; modern keeps them in the large or small header raw bytes.
    let (msg_id, cache_idx) = match file.function_headers.get(function_id as usize) {
        Some(FunctionHeader::Legacy(leg)) => {
            if leg.flags & crate::format::FLAG_OVERFLOWED != 0 {
                return Err(Error::Write(
                    "inject log: overflowed (large) legacy function headers not yet supported"
                        .into(),
                ));
            }
            let msg = leg.function_name;
            let cache = leg.highest_read_cache_index;
            if cache + 1 > u8::MAX as u32 {
                return Err(Error::Write("inject log: read cache full".into()));
            }
            let leg = match file.function_headers.get_mut(function_id as usize) {
                Some(FunctionHeader::Legacy(l)) => l,
                _ => unreachable!(),
            };
            leg.highest_read_cache_index = cache + 1;
            leg.frame_size = log_frame_size(leg.frame_size);
            (msg, cache)
        }
        Some(FunctionHeader::Modern(m)) => {
            let msg = m.function_name;
            let cache = reserve_modern_log_regs(file, function_id)?;
            (msg, cache)
        }
        None => {
            return Err(Error::Write(format!("invalid function id {function_id}")));
        }
    };
    if print_id > u16::MAX as u32 || msg_id > u16::MAX as u32 {
        return Err(Error::Write(
            "inject log: string id too large for short LoadConstString".into(),
        ));
    }

    let reg = |r: u8| Operand {
        ty: OperandType::Reg8,
        value: OperandValue::U8(r),
    };
    let u8v = |v: u8| Operand {
        ty: OperandType::UInt8,
        value: OperandValue::U8(v),
    };
    let u16v = |v: u16| Operand {
        ty: OperandType::UInt16,
        value: OperandValue::U16(v),
    };
    let mk = |opcode: u8, operands: Vec<Operand>| Instruction {
        offset: 0,
        opcode,
        operands,
        length: 0,
    };

    // r0=global, r1=print fn, r2=this(undefined), r3=message
    let mut seq = vec![
        mk(op_ggo, vec![reg(0)]),
        mk(op_try, vec![reg(1), reg(0), u8v(cache_idx as u8), u16v(print_id as u16)]),
        mk(op_lcu, vec![reg(2)]),
        mk(op_lcs, vec![reg(3), u16v(msg_id as u16)]),
        mk(op_call2, vec![reg(0), reg(1), reg(2), reg(3)]),
    ];

    // Keep the injected size a multiple of 4 so downstream functions shift by a
    // 4-aligned delta and their SwitchImm jump tables stay aligned. Pad with the
    // 1-byte AsyncBreakCheck (a runtime no-op) when available.
    if let Some(op_abc) = opc("AsyncBreakCheck") {
        let injected_len = encode_function_body(format, &seq)?.len();
        let pad = (4 - injected_len % 4) % 4;
        for _ in 0..pad {
            seq.push(mk(op_abc, vec![]));
        }
    }

    for (i, ins) in seq.into_iter().enumerate() {
        body.insert(i, ins);
    }
    Ok(())
}

// Inject a stub into a function body, then serialize.
//
// LogEntry injects a real `print(<function name>)` prologue (resize path).
// Prefer functions without overflow.
pub fn inject_stub(
    file: &mut BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    kind: InjectStubKind,
    options: &PatchOptions,
) -> Result<Vec<u8>> {
    let mut body = file.decode_function_instructions(format, function_id)?;
    match kind {
        InjectStubKind::NopPad => {
            // Find AsyncBreakCheck opcode by name if present; else no-op success with identity.
            if let Some(op) = format
                .definitions
                .iter()
                .find(|d| d.name == "AsyncBreakCheck")
                .map(|d| d.opcode)
            {
                // Insert before final Ret if present
                let insert_at = body
                    .iter()
                    .rposition(|i| {
                        format
                            .definitions
                            .get(i.opcode as usize)
                            .map(|d| d.name == "Ret")
                            .unwrap_or(false)
                    })
                    .unwrap_or(body.len());
                body.insert(
                    insert_at,
                    Instruction {
                        offset: 0,
                        opcode: op,
                        operands: vec![],
                        length: 1,
                    },
                );
            }
        }
        InjectStubKind::LogEntry => {
            build_log_entry(file, format, function_id, &mut body)?;
        }
    }
    // Recompute offsets in the instruction list for encode (encode ignores insn.offset).
    patch_function_body(file, format, function_id, &body, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write::footer::verify_footer;

    #[test]
    fn inject_stub_grows_and_reparses() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/react-native/v96/expressions/generator/bytecode.hbc"
        );
        if !std::path::Path::new(path).exists() {
            return;
        }
        let bytes = std::fs::read(path).unwrap();
        let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
        let format = BytecodeFormat::for_version(file.header.version).unwrap();
        let out =
            inject_stub(&mut file, &format, 0, InjectStubKind::NopPad, &PatchOptions::default())
                .unwrap();
        assert!(verify_footer(&out));
        BytecodeFile::parse_auto(&out).expect("reparse after inject");
    }

    // Modern (v97+) headers keep the body region contiguous with a 4 byte aligned
    // FunctionInfo region after it, so a resize has to shift the tail by a 4
    // aligned amount and relocate every overflowed large header. Exercise a nop
    // and a log stub on a real v98 file and confirm both still parse.
    #[test]
    fn inject_stub_modern_v98_grows_and_reparses() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/react-native/v98/expressions/class_basic/bytecode.hbc"
        );
        if !std::path::Path::new(path).exists() {
            return;
        }
        let bytes = std::fs::read(path).unwrap();
        let file0 = BytecodeFile::parse_auto(&bytes).unwrap();
        let format = BytecodeFormat::for_version(file0.header.version).unwrap();
        assert!(matches!(
            file0.header.function_header_layout,
            crate::format::FunctionHeaderLayout::Modern12
        ));

        for kind in [InjectStubKind::NopPad, InjectStubKind::LogEntry] {
            let mut file = file0.clone();
            let out = inject_stub(&mut file, &format, 0, kind, &PatchOptions::default()).unwrap();
            assert!(verify_footer(&out), "footer invalid for {kind:?}");
            let reparsed = BytecodeFile::parse_auto(&out)
                .unwrap_or_else(|e| panic!("reparse after modern inject {kind:?}: {e}"));
            // The body region and the FunctionInfo region that follows must both
            // still be reachable, so every function large header parses.
            assert_eq!(
                reparsed.function_headers.len(),
                file0.function_headers.len(),
                "function count changed for {kind:?}"
            );
        }
    }
}
