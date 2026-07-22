// Full `.hbc` image serialization.
//
// Primary path: identity re-emit from `BytecodeFile::raw_bytes` (preserves
// overflow headers, debug info, packing). Mutation paths update raw bytes then
// rehash the footer.
//
// Secondary path (`create`): build a minimal legacy-layout image from scratch.

use crate::error::{Error, Result};
use crate::file::BytecodeFile;
use crate::format::{HeaderLayout, FLAG_OVERFLOWED};
use crate::opcode::BytecodeFormat;

use super::footer::{append_footer, rehash_footer};
use super::header_write::{write_function_header_legacy_small, write_legacy_header};

#[derive(Debug, Clone, Default)]
pub struct SerializeOptions {
    // If true, drop debug info when writing (smaller / simpler). Currently a no-op
    // on the raw-preserving path; used by create.
    pub strip_debug_info: bool,
}

// Serialize a fully populated `BytecodeFile` to raw `.hbc` bytes.
//
// When `raw_bytes` is present, this is an identity (plus footer rehash) so
// overflowed function headers and debug info stay valid.
pub fn serialize_file(
    file: &BytecodeFile,
    _format: &BytecodeFormat,
    _options: &SerializeOptions,
) -> Result<Vec<u8>> {
    if let Some(raw) = &file.raw_bytes {
        let mut out = raw.clone();
        // Ensure file_length field matches actual length after rehash.
        rehash_footer(&mut out)?;
        // Update file_length in header (offset 32 for both layouts after magic+ver+hash).
        if out.len() >= 36 {
            let len = out.len() as u32;
            out[32..36].copy_from_slice(&len.to_le_bytes());
            // file_length is part of the hashed region, rehash again.
            rehash_footer(&mut out)?;
        }
        return Ok(out);
    }
    Err(Error::Write(
        "serialize_file: no raw_bytes on BytecodeFile, parse from disk first, or use create_minimal"
            .into(),
    ))
}

// Convenience: serialize and write to a path.
pub fn write_file(
    path: impl AsRef<std::path::Path>,
    file: &BytecodeFile,
    format: &BytecodeFormat,
    options: &SerializeOptions,
) -> Result<()> {
    let bytes = serialize_file(file, format, options)?;
    std::fs::write(path, bytes).map_err(Error::Io)
}

// Rebuild a complete image from `raw_bytes` after in-place mutations to the
// working buffer (function bodies / string storage). Always rehashes footer and
// updates file_length.
pub fn finalize_raw_image(mut buf: Vec<u8>) -> Result<Vec<u8>> {
    if buf.len() < 36 {
        return Err(Error::Write("image too small".into()));
    }
    // Provisional length without footer adjustment.
    let mut len = buf.len() as u32;
    // If trailing looks like a footer already, length includes it.
    buf[32..36].copy_from_slice(&len.to_le_bytes());
    rehash_footer(&mut buf)?;
    len = buf.len() as u32;
    buf[32..36].copy_from_slice(&len.to_le_bytes());
    rehash_footer(&mut buf)?;
    Ok(buf)
}

// ---- create-from-scratch helpers (legacy layout, non-overflow headers) ----

// Build a minimal v96-style legacy HBC: one global function with the given body
// and string table. Used by `create` and tests.
pub fn build_minimal_legacy(
    version: u32,
    strings: &[String],
    global_body: &[u8],
) -> Result<Vec<u8>> {
    if version >= 97 {
        return Err(Error::Write(
            "create_minimal: modern headers (v>=97) not yet supported, use v96 or lower".into(),
        ));
    }

    // Header is written last-pass with correct sizes; build body sections first.
    let mut string_storage = Vec::new();
    let mut small_entries = Vec::new();
    for s in strings {
        let offset = string_storage.len() as u32;
        let bytes = s.as_bytes();
        let length = bytes.len() as u32;
        if length >= 0xff || offset >= 0x800000 {
            return Err(Error::Write(
                "create: string too long for small-table entry (overflow table not implemented)"
                    .into(),
            ));
        }
        string_storage.extend_from_slice(bytes);
        // isUTF16=0 | offset<<1 | length<<24
        let raw = (length << 24) | ((offset & 0x7f_ffff) << 1);
        small_entries.push(raw);
    }

    // String kinds: one run of String kind covering all strings.
    let string_kind_raw: u32 = strings.len() as u32; // high bit 0 = String

    // Function header (legacy small, 16 bytes), offsets filled after layout.
    // We'll place bytecode right after tables.
    // Placeholder 128-byte header, filled after layout.
    let mut buf = vec![0u8; 128];

    // function headers: 1 function
    let func_header_pos = buf.len();
    buf.extend_from_slice(&[0u8; 16]); // filled later

    // string kinds (1 entry)
    buf.extend_from_slice(&string_kind_raw.to_le_bytes());
    align4(&mut buf);

    // identifier hashes: empty
    align4(&mut buf);

    // small string table
    for e in &small_entries {
        buf.extend_from_slice(&e.to_le_bytes());
    }
    align4(&mut buf);

    // overflow string table: empty
    align4(&mut buf);

    // string storage
    buf.extend_from_slice(&string_storage);
    align4(&mut buf);

    // array buffer empty, obj keys empty, obj values empty
    // bigints empty for version with bigint fields, still emit zero-size sections
    // regexp empty, cjs empty

    // For v96: after string storage:
    // array, obj_key, obj_value, bigint table, bigint storage, regexp table, regexp storage,
    // cjs, (function source if v>=84)

    // All empty sections still need correct alignment, sizes in header are 0 so we write nothing.

    if version >= 84 {
        // function_source_table empty
    }

    let instruction_offset = buf.len() as u32;
    buf.extend_from_slice(global_body);

    // Fill function header at func_header_pos
    let fh = write_function_header_legacy_small(
        instruction_offset,
        1, // param_count (this)
        global_body.len() as u32,
        0, // function_name string index "global" if present
        0, // info_offset
        2, // frame_size
        0, // env
        0,
        0,
        0x12, // flags: no-construct typical
    );
    buf[func_header_pos..func_header_pos + 16].copy_from_slice(&fh);

    // Write header fields
    write_legacy_header(
        &mut buf,
        version,
        [0u8; 20],
        0, // file_length filled later
        0, // global_code_index
        1, // function_count
        1, // string_kind_count
        0, // identifier_count
        strings.len() as u32,
        0, // overflow_string_count
        string_storage.len() as u32,
        version >= 87,
        0,
        0, // bigint
        0,
        0, // regexp
        0, // array
        0, // obj_key
        0, // obj_value
        version >= 78,
        0, // segment
        0, // cjs
        version >= 84,
        0, // function_source_count
        0, // debug_info_offset
        0, // options
    )?;

    append_footer(&mut buf)?;
    let len = buf.len() as u32;
    buf[32..36].copy_from_slice(&len.to_le_bytes());
    rehash_footer(&mut buf)?;
    Ok(buf)
}

// Build a minimal Modern (v97+) HBC: one overflowed global function with the given
// body and string table. Every real v98 function is overflowed (the 12 byte small
// header is only a pointer to an out-of-line large header), so this mirrors what
// hermesc emits and exercises the layout the runtime actually validates. The
// section order is function_headers, string_kinds, identifier_hashes,
// small_string_table, overflow_string_table, string_storage, then the empty
// buffer/table sections, then the bytecode, then the large header.
pub fn build_minimal_modern(
    version: u32,
    strings: &[String],
    global_body: &[u8],
) -> Result<Vec<u8>> {
    use super::header_write::write_modern_header;

    if version < 97 {
        return Err(Error::Write(
            "build_minimal_modern: version must be 97 or newer".into(),
        ));
    }

    let mut string_storage = Vec::new();
    let mut small_entries = Vec::new();
    for s in strings {
        let offset = string_storage.len() as u32;
        let bytes = s.as_bytes();
        let length = bytes.len() as u32;
        if length >= 0xff || offset >= 0x80_0000 {
            return Err(Error::Write(
                "create: string too long for small-table entry (overflow table not implemented)"
                    .into(),
            ));
        }
        string_storage.extend_from_slice(bytes);
        // isUTF16=0 | offset<<1 | length<<24
        let raw = (length << 24) | ((offset & 0x7f_ffff) << 1);
        small_entries.push(raw);
    }
    let string_kind_raw: u32 = strings.len() as u32; // high bit 0 = String

    // Placeholder 128 byte header, filled after layout.
    let mut buf = vec![0u8; 128];

    // function headers: 1 function (modern 12 byte small header), filled later.
    let func_header_pos = buf.len();
    buf.extend_from_slice(&[0u8; 12]);
    align4(&mut buf);

    // string kinds (1 entry)
    buf.extend_from_slice(&string_kind_raw.to_le_bytes());
    align4(&mut buf);

    // identifier hashes: empty
    align4(&mut buf);

    // small string table
    for e in &small_entries {
        buf.extend_from_slice(&e.to_le_bytes());
    }
    align4(&mut buf);

    // overflow string table: empty
    align4(&mut buf);

    // string storage
    buf.extend_from_slice(&string_storage);
    align4(&mut buf);

    // Empty modern buffer/table sections (literal_value_buffer, obj_key_buffer,
    // obj_shape_table, bigints, regexp, cjs, function_source) are all zero-sized.

    let instruction_offset = buf.len() as u32;
    buf.extend_from_slice(global_body);
    align4(&mut buf);

    // Out-of-line large header (FunctionInfo), 4 byte aligned. Field order matches
    // the parser: offset, param, loop_depth, size, name, numReg, nonPtrReg, frame,
    // then read/write/newObj/privateName caches and the flags byte.
    let large_header_pos = buf.len() as u32;
    let large: [u32; 8] = [
        instruction_offset,       // body offset
        1,                        // param_count (this)
        0,                        // loop_depth
        global_body.len() as u32, // bytecode_size
        0,                        // function_name string index "global"
        0,                        // number_reg_count
        1,                        // non_ptr_reg_count (r0 holds undefined)
        1,                        // frame_size
    ];
    for v in large {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    // read/write/newObj/private caches = 0, then the flags byte. In Hermes the low
    // two bits are ProhibitInvoke where 0 means ProhibitCall (must use `new`), so a
    // plain callable global needs ProhibitNone, value 2.
    const PROHIBIT_NONE: u8 = 0b10;
    buf.extend_from_slice(&[0u8, 0, 0, 0, PROHIBIT_NONE]);
    align4(&mut buf);

    // Small header points at the large header. large_ptr = (name << 24) | (ptr &
    // 0xffffff); the overflow bit (0x20) sits in the flags byte at byte 11.
    let mut small = [0u8; 12];
    let raw: u128 = ((large_header_pos as u128) & 0x00ff_ffff)
        | (((large_header_pos as u128) >> 24) << 46)
        | ((crate::format::FLAG_OVERFLOWED as u128) << 88);
    small.copy_from_slice(&raw.to_le_bytes()[..12]);
    buf[func_header_pos..func_header_pos + 12].copy_from_slice(&small);

    write_modern_header(
        &mut buf,
        version,
        [0u8; 20],
        1, // function_count
        1, // string_kind_count
        0, // identifier_count
        strings.len() as u32,
        string_storage.len() as u32,
    )?;

    append_footer(&mut buf)?;
    let len = buf.len() as u32;
    buf[32..36].copy_from_slice(&len.to_le_bytes());
    rehash_footer(&mut buf)?;
    Ok(buf)
}

fn align4(buf: &mut Vec<u8>) {
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
}

// Ensure a file has raw_bytes attached (clone instructions path fallback).
pub fn ensure_raw(file: &mut BytecodeFile) -> Result<()> {
    if file.raw_bytes.is_some() {
        return Ok(());
    }
    Err(Error::Write(
        "BytecodeFile has no raw_bytes; re-parse the input with parse_auto".into(),
    ))
}

pub fn section_offset(file: &BytecodeFile, name: &str) -> Option<u32> {
    file.sections
        .iter()
        .find(|s| s.name == name)
        .map(|s| s.offset)
}

// Check whether any function header is overflowed (large header out-of-line).
pub fn has_overflowed_functions(file: &BytecodeFile) -> bool {
    file.function_headers.iter().any(|h| match h {
        crate::format::FunctionHeader::Legacy(l) => l.flags & FLAG_OVERFLOWED != 0,
        crate::format::FunctionHeader::Modern(m) => m.flags & FLAG_OVERFLOWED != 0,
    })
}

pub fn layout_is_legacy(file: &BytecodeFile) -> bool {
    matches!(file.header.layout, HeaderLayout::Legacy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::BytecodeFile;
    use crate::opcode::BytecodeFormat;
    use crate::write::footer::verify_footer;

    #[test]
    fn identity_serialize_v96() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/react-native/v96/expressions/generator/bytecode.hbc"
        );
        if !std::path::Path::new(path).exists() {
            return;
        }
        let bytes = std::fs::read(path).unwrap();
        let file = BytecodeFile::parse_auto(&bytes).unwrap();
        let format = BytecodeFormat::for_version(file.header.version).unwrap();
        let out = serialize_file(&file, &format, &SerializeOptions::default()).unwrap();
        assert!(verify_footer(&out));
        // Re-parse must succeed
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert_eq!(re.header.function_count, file.header.function_count);
        assert_eq!(re.strings.len(), file.strings.len());
    }
}
