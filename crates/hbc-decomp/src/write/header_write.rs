// Binary writers for HBC headers (legacy layout first).

use crate::error::{Error, Result};

const MAGIC: u64 = 0x1F1903C103BC1FC6;

// Write a Legacy16 small function header (16 bytes). The many fields mirror the
// on-disk header layout one to one, so they stay as positional arguments.
#[allow(clippy::too_many_arguments)]
pub fn write_function_header_legacy_small(
    offset: u32,
    param_count: u32,
    bytecode_size: u32,
    function_name: u32,
    info_offset: u32,
    frame_size: u32,
    environment_size: u32,
    highest_read_cache: u32,
    highest_write_cache: u32,
    flags: u8,
) -> [u8; 16] {
    let mut raw: u128 = 0;
    raw |= (offset as u128) & ((1u128 << 25) - 1);
    raw |= ((param_count as u128) & ((1u128 << 7) - 1)) << 25;
    raw |= ((bytecode_size as u128) & ((1u128 << 15) - 1)) << 32;
    raw |= ((function_name as u128) & ((1u128 << 17) - 1)) << 47;
    raw |= ((info_offset as u128) & ((1u128 << 25) - 1)) << 64;
    raw |= ((frame_size as u128) & ((1u128 << 7) - 1)) << 89;
    raw |= ((environment_size as u128) & 0xff) << 96;
    raw |= ((highest_read_cache as u128) & 0xff) << 104;
    raw |= ((highest_write_cache as u128) & 0xff) << 112;
    raw |= (flags as u128) << 120;
    raw.to_le_bytes()
}

// Write the fixed part of a Modern (v97+) 128 byte file header. Sizes/counts are
// filled from the caller; `file_length` is patched after the footer is appended.
#[allow(clippy::too_many_arguments)]
pub fn write_modern_header(
    buf: &mut [u8],
    version: u32,
    source_hash: [u8; 20],
    function_count: u32,
    string_kind_count: u32,
    identifier_count: u32,
    string_count: u32,
    string_storage_size: u32,
) -> Result<()> {
    if buf.len() < 128 {
        return Err(Error::Write("modern header buffer too small".into()));
    }
    buf[0..8].copy_from_slice(&MAGIC.to_le_bytes());
    buf[8..12].copy_from_slice(&version.to_le_bytes());
    buf[12..32].copy_from_slice(&source_hash);
    // Every u32 field in order after the source hash. file_length (offset 32) is
    // patched after the footer; the zeros are the empty buffer/table sections.
    let fields: [u32; 19] = [
        function_count,     // 40
        string_kind_count,  // 44
        identifier_count,   // 48
        string_count,       // 52
        0,                  // 56 overflow_string_count
        string_storage_size, // 60
        0,                  // 64 big_int_count
        0,                  // 68 big_int_storage_size
        0,                  // 72 reg_exp_count
        0,                  // 76 reg_exp_storage_size
        0,                  // 80 literal_value_buffer_size
        0,                  // 84 obj_key_buffer_size
        0,                  // 88 obj_shape_table_count
        0,                  // 92 num_string_switch_imms
        0,                  // 96 segment_id
        0,                  // 100 cjs_module_count
        0,                  // 104 function_source_count
        0,                  // 108 debug_info_offset
        0,                  // 112 options (only low byte is read)
    ];
    for (i, v) in fields.iter().enumerate() {
        let off = 40 + i * 4;
        buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
    Ok(())
}

// Patch a legacy small function header's offset + size fields in a 16-byte slot.
pub fn patch_legacy_small_header_offset_size(
    slot: &mut [u8],
    offset: u32,
    bytecode_size: u32,
) -> Result<()> {
    if slot.len() < 16 {
        return Err(Error::Write("function header slot too small".into()));
    }
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&slot[..16]);
    let mut raw = u128::from_le_bytes(bytes);
    // clear offset (25 bits) and size (15 bits at 32)
    raw &= !((1u128 << 25) - 1);
    raw &= !(((1u128 << 15) - 1) << 32);
    raw |= (offset as u128) & ((1u128 << 25) - 1);
    raw |= ((bytecode_size as u128) & ((1u128 << 15) - 1)) << 32;
    let out = raw.to_le_bytes();
    slot[..16].copy_from_slice(&out);
    Ok(())
}

// Shift the body `offset` field (bits 0..24) of a Modern12 function header by
// `delta`, reading and rewriting the field in place so it does not depend on
// any decoded value. Only valid for non-overflowed modern headers, where this
// field is the real body offset.
pub fn shift_modern_small_header_offset(slot: &mut [u8], delta: i64) -> Result<()> {
    if slot.len() < 12 {
        return Err(Error::Write("modern function header slot too small".into()));
    }
    let mut bytes = [0u8; 16];
    bytes[..12].copy_from_slice(&slot[..12]);
    let mut raw = u128::from_le_bytes(bytes);
    let cur = (raw & ((1u128 << 25) - 1)) as i64;
    let new_offset = (cur + delta) as u128 & ((1u128 << 25) - 1);
    raw &= !((1u128 << 25) - 1);
    raw |= new_offset;
    let out = raw.to_le_bytes();
    slot[..12].copy_from_slice(&out[..12]);
    Ok(())
}

// Shift the body offset (bits 0..24) and, when set, the info offset (bits
// 64..88) of a non-overflowed Legacy16 small header by `delta`, in place.
pub fn shift_legacy_small_header_offsets(slot: &mut [u8], delta: i64) -> Result<()> {
    if slot.len() < 16 {
        return Err(Error::Write("function header slot too small".into()));
    }
    let mut raw = u128::from_le_bytes(slot[..16].try_into().unwrap());
    let off = (raw & ((1u128 << 25) - 1)) as i64;
    let info = ((raw >> 64) & ((1u128 << 25) - 1)) as i64;
    let new_off = (off + delta) as u128 & ((1u128 << 25) - 1);
    raw &= !((1u128 << 25) - 1);
    raw |= new_off;
    if info != 0 {
        let new_info = (info + delta) as u128 & ((1u128 << 25) - 1);
        raw &= !(((1u128 << 25) - 1) << 64);
        raw |= new_info << 64;
    }
    slot[..16].copy_from_slice(&raw.to_le_bytes());
    Ok(())
}

// Read the body offset (bits 0..24) of a Legacy16 large header packed pointer:
// large_ptr = (info_offset << 16) | offset, where offset is bits 0..24 and
// info_offset is bits 64..88 of the small header.
pub fn read_legacy_large_pointer(slot: &[u8]) -> Result<u32> {
    if slot.len() < 16 {
        return Err(Error::Write("function header slot too small".into()));
    }
    let raw = u128::from_le_bytes(slot[..16].try_into().unwrap());
    let offset = (raw & 0xffff) as u32;
    let info = ((raw >> 64) & ((1u128 << 25) - 1)) as u32;
    Ok((info << 16) | offset)
}

// Rewrite the packed large-header pointer of an overflowed Legacy16 small header,
// shifting it by `delta` (offset = ptr & 0xffff in bits 0..24, info = ptr >> 16
// in bits 64..88).
pub fn shift_legacy_large_pointer(slot: &mut [u8], delta: i64) -> Result<()> {
    let cur = read_legacy_large_pointer(slot)? as i64;
    let new_ptr = (cur + delta) as u64;
    let mut raw = u128::from_le_bytes(slot[..16].try_into().unwrap());
    raw &= !((1u128 << 25) - 1); // clear offset field
    raw &= !(((1u128 << 25) - 1) << 64); // clear info field
    raw |= (new_ptr & 0xffff) as u128;
    raw |= (((new_ptr >> 16) & ((1 << 25) - 1)) as u128) << 64;
    slot[..16].copy_from_slice(&raw.to_le_bytes());
    Ok(())
}

// Read the packed large-header pointer of an overflowed Modern12 small header:
// large_ptr = (function_name << 24) | (offset & 0x00ff_ffff).
pub fn read_modern_large_pointer(slot: &[u8]) -> Result<u32> {
    if slot.len() < 12 {
        return Err(Error::Write("modern function header slot too small".into()));
    }
    let mut bytes = [0u8; 16];
    bytes[..12].copy_from_slice(&slot[..12]);
    let raw = u128::from_le_bytes(bytes);
    let offset = (raw & 0x00ff_ffff) as u32;
    let name = ((raw >> 46) & 0xff) as u32;
    Ok((name << 24) | offset)
}

// Rewrite the packed large-header pointer of an overflowed Modern12 small header,
// shifting it by `delta`. Updates the offset field (low 24 bits) and the
// function_name field (bits 46..53), leaving every other field untouched.
pub fn shift_modern_large_pointer(slot: &mut [u8], delta: i64) -> Result<()> {
    if slot.len() < 12 {
        return Err(Error::Write("modern function header slot too small".into()));
    }
    let cur = read_modern_large_pointer(slot)? as i64;
    let new_ptr = (cur + delta) as u64;
    let mut bytes = [0u8; 16];
    bytes[..12].copy_from_slice(&slot[..12]);
    let mut raw = u128::from_le_bytes(bytes);
    // clear the low 24 offset bits and the 8-bit function_name field (bits 46..53)
    raw &= !0x00ff_ffffu128;
    raw &= !(0xffu128 << 46);
    raw |= (new_ptr as u128) & 0x00ff_ffff;
    raw |= (((new_ptr >> 24) & 0xff) as u128) << 46;
    let out = raw.to_le_bytes();
    slot[..12].copy_from_slice(&out[..12]);
    Ok(())
}

// Add `delta` to a little-endian u32 stored at `buf[pos..pos+4]`.
pub fn shift_u32_at(buf: &mut [u8], pos: usize, delta: i64) -> Result<()> {
    if pos + 4 > buf.len() {
        return Err(Error::Write("shift_u32_at out of range".into()));
    }
    let cur = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as i64;
    let new = (cur + delta) as u32;
    buf[pos..pos + 4].copy_from_slice(&new.to_le_bytes());
    Ok(())
}

// Write fields into the first 128 bytes of `buf` (legacy header layout).
#[allow(clippy::too_many_arguments)]
pub fn write_legacy_header(
    buf: &mut [u8],
    version: u32,
    source_hash: [u8; 20],
    file_length: u32,
    global_code_index: u32,
    function_count: u32,
    string_kind_count: u32,
    identifier_count: u32,
    string_count: u32,
    overflow_string_count: u32,
    string_storage_size: u32,
    has_bigint: bool,
    big_int_count: u32,
    big_int_storage_size: u32,
    reg_exp_count: u32,
    reg_exp_storage_size: u32,
    array_buffer_size: u32,
    obj_key_buffer_size: u32,
    obj_value_buffer_size: u32,
    has_segment_id: bool,
    segment_id: u32,
    cjs_module_count: u32,
    has_function_source: bool,
    function_source_count: u32,
    debug_info_offset: u32,
    options: u8,
) -> Result<()> {
    if buf.len() < 128 {
        return Err(Error::Write("buffer too small for header".into()));
    }
    buf[0..8].copy_from_slice(&MAGIC.to_le_bytes());
    buf[8..12].copy_from_slice(&version.to_le_bytes());
    buf[12..32].copy_from_slice(&source_hash);
    buf[32..36].copy_from_slice(&file_length.to_le_bytes());
    buf[36..40].copy_from_slice(&global_code_index.to_le_bytes());
    buf[40..44].copy_from_slice(&function_count.to_le_bytes());
    buf[44..48].copy_from_slice(&string_kind_count.to_le_bytes());
    buf[48..52].copy_from_slice(&identifier_count.to_le_bytes());
    buf[52..56].copy_from_slice(&string_count.to_le_bytes());
    buf[56..60].copy_from_slice(&overflow_string_count.to_le_bytes());
    buf[60..64].copy_from_slice(&string_storage_size.to_le_bytes());
    let mut pos = 64;
    if has_bigint {
        buf[pos..pos + 4].copy_from_slice(&big_int_count.to_le_bytes());
        pos += 4;
        buf[pos..pos + 4].copy_from_slice(&big_int_storage_size.to_le_bytes());
        pos += 4;
    }
    buf[pos..pos + 4].copy_from_slice(&reg_exp_count.to_le_bytes());
    pos += 4;
    buf[pos..pos + 4].copy_from_slice(&reg_exp_storage_size.to_le_bytes());
    pos += 4;
    buf[pos..pos + 4].copy_from_slice(&array_buffer_size.to_le_bytes());
    pos += 4;
    buf[pos..pos + 4].copy_from_slice(&obj_key_buffer_size.to_le_bytes());
    pos += 4;
    buf[pos..pos + 4].copy_from_slice(&obj_value_buffer_size.to_le_bytes());
    pos += 4;
    if has_segment_id {
        buf[pos..pos + 4].copy_from_slice(&segment_id.to_le_bytes());
        pos += 4;
    } else {
        // cjs_module_offset field on older versions
        buf[pos..pos + 4].copy_from_slice(&0u32.to_le_bytes());
        pos += 4;
    }
    buf[pos..pos + 4].copy_from_slice(&cjs_module_count.to_le_bytes());
    pos += 4;
    if has_function_source {
        buf[pos..pos + 4].copy_from_slice(&function_source_count.to_le_bytes());
        pos += 4;
    }
    buf[pos..pos + 4].copy_from_slice(&debug_info_offset.to_le_bytes());
    pos += 4;
    buf[pos] = options;
    Ok(())
}
