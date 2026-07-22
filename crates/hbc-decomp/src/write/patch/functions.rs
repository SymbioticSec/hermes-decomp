// Replace whole function bodies. Same-size bodies patch in place; different-size
// bodies splice the code section, shift later function offsets, and fix the
// debug-info offset. Legacy, non-overflowed headers.

use crate::error::{Error, Result};
use crate::file::{BytecodeFile, Instruction};
use crate::format::FunctionHeader;
use crate::opcode::BytecodeFormat;

use crate::write::encode::encode_function_body;
use crate::write::serialize::{finalize_raw_image, section_offset};

use super::strings::legacy_debug_info_offset_pos;
use super::PatchOptions;

// Replace the instruction stream of `function_id`. Same-size bodies patch in place;
// longer bodies expand the code section and shift subsequent function offsets.
pub fn patch_function_body(
    file: &mut BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    new_body: &[Instruction],
    _options: &PatchOptions,
) -> Result<Vec<u8>> {
    let old_size = file
        .function_headers
        .get(function_id as usize)
        .map(|h| h.bytecode_size_in_bytes() as usize)
        .unwrap_or(0);
    let encoded = encode_function_body(format, new_body)?;
    let delta = encoded.len() as i64 - old_size as i64;

    // Keep the size delta a multiple of 4 so the FunctionInfo region that follows
    // the code stays 4 byte aligned. Pad with the 1 byte AsyncBreakCheck (a runtime
    // no-op) inserted just before the terminator so the function still ends on a
    // terminating instruction.
    if delta != 0 && delta.rem_euclid(4) != 0 {
        if let Some(op_abc) = format
            .definitions
            .iter()
            .find(|d| d.name == "AsyncBreakCheck")
            .map(|d| d.opcode)
        {
            let pad = (4 - delta.rem_euclid(4)) as usize;
            let mut padded = new_body.to_vec();
            let insert_at = padded.len().saturating_sub(1);
            for _ in 0..pad {
                padded.insert(
                    insert_at,
                    Instruction {
                        offset: 0,
                        opcode: op_abc,
                        operands: vec![],
                        length: 1,
                    },
                );
            }
            let encoded = encode_function_body(format, &padded)?;
            return patch_function_bytes(file, function_id, &encoded);
        }
    }
    patch_function_bytes(file, function_id, &encoded)
}

// Low-level: replace function body with raw bytes.
pub fn patch_function_bytes(
    file: &mut BytecodeFile,
    function_id: u32,
    new_body: &[u8],
) -> Result<Vec<u8>> {
    let (old_size, abs_off, patched_offset) = {
        let fh = file
            .function_headers
            .get(function_id as usize)
            .ok_or_else(|| Error::Write(format!("invalid function id {function_id}")))?;
        (
            fh.bytecode_size_in_bytes() as usize,
            fh.offset() as usize,
            fh.offset(),
        )
    };
    let mut raw = file
        .raw_bytes
        .clone()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;

    if new_body.len() == old_size {
        if abs_off + old_size > raw.len() {
            return Err(Error::Write("function body out of range".into()));
        }
        raw[abs_off..abs_off + old_size].copy_from_slice(new_body);
        // Update instructions cache
        let rel = abs_off
            .checked_sub(file.instruction_offset as usize)
            .ok_or_else(|| Error::Write("offset underflow".into()))?;
        if rel + old_size <= file.instructions.len() {
            file.instructions[rel..rel + old_size].copy_from_slice(new_body);
        }
        let out = finalize_raw_image(raw)?;
        file.raw_bytes = Some(out.clone());
        return Ok(out);
    }

    // Grow / shrink. Function bodies form one contiguous region followed by the
    // FunctionInfo region (large headers, exception tables, debug info), which is
    // 4 byte aligned. Callers align the body delta to a multiple of 4 so the whole
    // tail shifts by a 4 aligned amount and every large header stays aligned.
    let delta = new_body.len() as i64 - old_size as i64;
    if abs_off + old_size > raw.len() {
        return Err(Error::Write("function body out of range".into()));
    }

    // Splice body
    let mut rebuilt = Vec::with_capacity((raw.len() as i64 + delta) as usize);
    rebuilt.extend_from_slice(&raw[..abs_off]);
    rebuilt.extend_from_slice(new_body);
    rebuilt.extend_from_slice(&raw[abs_off + old_size..]);

    // Patch function headers section: update this function size and all later offsets.
    let fh_sec = section_offset(file, "function_headers")
        .ok_or_else(|| Error::Write("function_headers section missing".into()))?
        as usize;
    let header_size = match file.header.function_header_layout {
        crate::format::FunctionHeaderLayout::Legacy16 => 16,
        crate::format::FunctionHeaderLayout::Modern12 => 12,
    };

    // Everything at or after the end of the patched body moved by `delta`. For
    // each function we shift its body offset (except the patched one, whose body
    // did not move but whose size changed) and, when overflowed, relocate the
    // large header and its internal offset / size / info fields.
    let threshold = abs_off + old_size;
    let modern = header_size == 12;
    for i in 0..file.function_headers.len() {
        let slot = fh_sec + i * header_size;
        if slot + header_size > rebuilt.len() {
            break;
        }
        let flag_byte = if modern { 11 } else { 15 };
        let overflowed = rebuilt[slot + flag_byte] & crate::format::FLAG_OVERFLOWED != 0;
        let is_target = i as u32 == function_id;
        if overflowed {
            resize_overflowed_function(
                &mut rebuilt,
                slot,
                modern,
                threshold,
                delta,
                is_target.then_some(new_body.len() as u32),
            )?;
        } else if modern {
            // Body offset lives in the 12-byte header (bits 0..24) and size in
            // bits 32..45.
            resize_modern_small(
                &mut rebuilt[slot..slot + 12],
                threshold,
                delta,
                is_target.then_some(new_body.len() as u32),
            )?;
        } else {
            let leg = match &file.function_headers[i] {
                FunctionHeader::Legacy(l) => l,
                _ => unreachable!(),
            };
            let new_offset = if leg.offset as usize >= threshold {
                (leg.offset as i64 + delta) as u32
            } else {
                leg.offset
            };
            let new_size = if is_target {
                new_body.len() as u32
            } else {
                leg.bytecode_size_in_bytes
            };
            let new_info = if leg.info_offset != 0 && leg.info_offset as usize >= threshold {
                (leg.info_offset as i64 + delta) as u32
            } else {
                leg.info_offset
            };
            let bytes = crate::write::header_write::write_function_header_legacy_small(
                new_offset,
                leg.param_count,
                new_size,
                leg.function_name,
                new_info,
                leg.frame_size,
                leg.environment_size,
                leg.highest_read_cache_index,
                leg.highest_write_cache_index,
                leg.flags,
            );
            rebuilt[slot..slot + 16].copy_from_slice(&bytes);
        }
    }
    let _ = patched_offset;

    // The debug info section sits after the code, so its header offset shifts too.
    if file.header.debug_info_offset != 0 {
        let dpos = if modern {
            108
        } else {
            legacy_debug_info_offset_pos(&file.header)
        };
        let shifted = (file.header.debug_info_offset as i64 + delta) as u32;
        if dpos + 4 <= rebuilt.len() {
            rebuilt[dpos..dpos + 4].copy_from_slice(&shifted.to_le_bytes());
        }
        file.header.debug_info_offset = shifted;
    }

    // Update instruction cache roughly
    file.instructions = rebuilt[file.instruction_offset as usize..].to_vec();
    // Drop footer if present from old slice, finalize will rehash
    let out = finalize_raw_image(rebuilt)?;
    file.raw_bytes = Some(out.clone());
    Ok(out)
}

// Shift the body offset (bits 0..24) of a non-overflowed Modern12 small header
// when it sits at or past `threshold`, and optionally set the size (bits 32..45).
fn resize_modern_small(
    slot: &mut [u8],
    threshold: usize,
    delta: i64,
    new_size: Option<u32>,
) -> Result<()> {
    if slot.len() < 12 {
        return Err(Error::Write("modern header slot too small".into()));
    }
    let mut bytes = [0u8; 16];
    bytes[..12].copy_from_slice(&slot[..12]);
    let mut raw = u128::from_le_bytes(bytes);
    let off = (raw & ((1u128 << 25) - 1)) as usize;
    if off >= threshold {
        let new_off = (off as i64 + delta) as u128 & ((1u128 << 25) - 1);
        raw = (raw & !((1u128 << 25) - 1)) | new_off;
    }
    if let Some(sz) = new_size {
        raw &= !(((1u128 << 14) - 1) << 32);
        raw |= ((sz as u128) & ((1u128 << 14) - 1)) << 32;
    }
    let out = raw.to_le_bytes();
    slot[..12].copy_from_slice(&out[..12]);
    Ok(())
}

// Relocate an overflowed function during a body resize: shift the small header
// pointer and the large header's body offset when they sit past `threshold`, set
// the size when this is the patched function, and shift the legacy info offset.
fn resize_overflowed_function(
    rebuilt: &mut [u8],
    slot: usize,
    modern: bool,
    threshold: usize,
    delta: i64,
    new_size: Option<u32>,
) -> Result<()> {
    use crate::write::header_write as hw;
    let large_ptr = if modern {
        hw::read_modern_large_pointer(&rebuilt[slot..slot + 12])?
    } else {
        hw::read_legacy_large_pointer(&rebuilt[slot..slot + 16])?
    } as usize;
    let new_lh = if large_ptr >= threshold {
        if modern {
            hw::shift_modern_large_pointer(&mut rebuilt[slot..slot + 12], delta)?;
        } else {
            hw::shift_legacy_large_pointer(&mut rebuilt[slot..slot + 16], delta)?;
        }
        (large_ptr as i64 + delta) as usize
    } else {
        large_ptr
    };
    if new_lh + 16 > rebuilt.len() {
        return Err(Error::Write("large header out of range".into()));
    }
    // Body offset is the first u32; shift it if the body moved.
    let body_off = u32::from_le_bytes(rebuilt[new_lh..new_lh + 4].try_into().unwrap()) as usize;
    if body_off >= threshold {
        hw::shift_u32_at(rebuilt, new_lh, delta)?;
    }
    // Size field: legacy at +8, modern at +12.
    if let Some(sz) = new_size {
        let size_pos = new_lh + if modern { 12 } else { 8 };
        rebuilt[size_pos..size_pos + 4].copy_from_slice(&sz.to_le_bytes());
    }
    // Legacy large headers keep info_offset at +16.
    if !modern {
        let info_pos = new_lh + 16;
        let info = u32::from_le_bytes(rebuilt[info_pos..info_pos + 4].try_into().unwrap()) as usize;
        if info != 0 && info >= threshold {
            hw::shift_u32_at(rebuilt, info_pos, delta)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write::footer::verify_footer;

    #[test]
    fn patch_function_same_size_roundtrip() {
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
        let body = file.decode_function_instructions(&format, 0).unwrap();
        let out =
            patch_function_body(&mut file, &format, 0, &body, &PatchOptions::default()).unwrap();
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        let body2 = re.decode_function_instructions(&format, 0).unwrap();
        assert_eq!(body.len(), body2.len());
    }
}
