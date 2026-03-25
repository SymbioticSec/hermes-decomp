use crate::debug::try_parse_debug_info;
use crate::error::{Error, Result};
use crate::file::structure::{ShapeTableEntry, StringKindEntry, TableEntry};
use crate::file::{BytecodeFile, ExceptionHandler, SectionInfo};
use crate::format::{BytecodeHeader, FunctionHeader, FunctionHeaderLayout, HeaderLayout};
use crate::io::ByteReader;
use std::collections::BTreeMap;

use super::function::*;
use super::header::*;
use super::table::*;

// --- Helper types ---

// Shared tables parsed before the layout-specific sections.
struct CommonTables {
    function_headers: Vec<FunctionHeader>,
    string_kinds: Vec<StringKindEntry>,
    identifier_hashes: Vec<u32>,
    small_string_table: Vec<u32>,
    overflow_string_table: Vec<(u32, u32)>,
    string_storage: Vec<u8>,
}

// Layout-specific buffer data.
struct LayoutBuffers {
    big_int_table: Vec<TableEntry>,
    big_int_storage: Vec<u8>,
    array_buffer: Vec<u8>,
    literal_value_buffer: Vec<u8>,
    obj_key_buffer: Vec<u8>,
    obj_value_buffer: Vec<u8>,
    obj_shape_table: Vec<ShapeTableEntry>,
}

// --- Helper function ---

// Parse a section: record its start position, call the parser, align to 4 bytes,
// and push a SectionInfo entry. Returns the parser's result.
fn track_section<T>(
    reader: &mut ByteReader<'_>,
    sections: &mut Vec<SectionInfo>,
    name: &'static str,
    entries: Option<u32>,
    parse: impl FnOnce(&mut ByteReader<'_>) -> Result<T>,
) -> Result<T> {
    let sec_start = reader.position() as u32;
    let result = parse(reader)?;
    reader.align(4)?;
    sections.push(SectionInfo {
        name,
        offset: sec_start,
        size: reader.position() as u32 - sec_start,
        entries,
    });
    Ok(result)
}

// --- Entry points ---

pub fn parse_auto(bytes: &[u8]) -> Result<BytecodeFile> {
    let version = peek_version(bytes)?;
    let legacy =
        parse_with_layout(bytes, HeaderLayout::Legacy, FunctionHeaderLayout::Legacy16).ok();
    let modern =
        parse_with_layout(bytes, HeaderLayout::Modern, FunctionHeaderLayout::Modern12).ok();

    match (legacy, modern) {
        (Some(file), None) => Ok(file),
        (None, Some(file)) => Ok(file),
        (Some(legacy_file), Some(modern_file)) => {
            if version >= MODERN_FUNCTION_HEADER_MIN_VERSION {
                Ok(modern_file)
            } else {
                Ok(legacy_file)
            }
        }
        (None, None) => Err(Error::Parse(
            "failed to parse bytecode file using known layouts".to_string(),
        )),
    }
}

pub fn parse_with_layout(
    bytes: &[u8],
    layout: HeaderLayout,
    function_layout: FunctionHeaderLayout,
) -> Result<BytecodeFile> {
    if bytes.len() < HEADER_SIZE {
        return Err(Error::Parse("file too small for header".to_string()));
    }
    let mut reader = ByteReader::new(bytes);
    let header_start = reader.position();
    let mut sections = Vec::new();

    // Validate magic and parse header
    let magic = reader.read_u64()?;
    if magic != MAGIC {
        return Err(Error::Parse(format!(
            "invalid magic header: expected {MAGIC:#x} got {magic:#x}"
        )));
    }
    let version = reader.read_u32()?;
    let source_hash = {
        let hash_bytes = reader.read_bytes(20)?;
        let mut hash = [0u8; 20];
        hash.copy_from_slice(hash_bytes);
        hash
    };

    let mut header = match layout {
        HeaderLayout::Legacy => parse_legacy_header(&mut reader, version, magic, source_hash)?,
        HeaderLayout::Modern => parse_modern_header(&mut reader, version, magic, source_hash)?,
    };
    header.function_header_layout = function_layout;

    sections.push(SectionInfo {
        name: "header",
        offset: header_start as u32,
        size: HEADER_SIZE as u32,
        entries: None,
    });

    reader.seek(header_start + HEADER_SIZE)?;
    reader.align(4)?;

    // Parse common sections (shared by both layouts)
    let tables = parse_common_tables(&mut reader, &header, &mut sections)?;

    // Parse layout-specific buffers
    let buffers = match &header.layout {
        HeaderLayout::Legacy => parse_legacy_buffers(&mut reader, &header, &mut sections)?,
        HeaderLayout::Modern => parse_modern_buffers(&mut reader, &header, &mut sections)?,
    };

    // Parse trailing sections and build final result
    parse_trailing_and_build(bytes, &mut reader, sections, header, tables, buffers)
}

// Parse sections shared by both Legacy and Modern layouts:
// function headers, string kinds, identifier hashes, string tables, and string storage.
fn parse_common_tables(
    reader: &mut ByteReader<'_>,
    header: &BytecodeHeader,
    sections: &mut Vec<SectionInfo>,
) -> Result<CommonTables> {
    let function_headers = track_section(reader, sections, "function_headers",
        Some(header.function_count), |r| parse_function_headers(r, header))?;

    let string_kinds = track_section(reader, sections, "string_kinds",
        Some(header.string_kind_count), |r| parse_string_kinds(r, header.string_kind_count))?;

    let identifier_hashes = track_section(reader, sections, "identifier_hashes",
        Some(header.identifier_count), |r| parse_u32_vec(r, header.identifier_count))?;

    let small_string_table = track_section(reader, sections, "small_string_table",
        Some(header.string_count), |r| parse_u32_vec(r, header.string_count))?;

    let overflow_string_table = track_section(reader, sections, "overflow_string_table",
        Some(header.overflow_string_count),
        |r| parse_overflow_string_table(r, header.overflow_string_count))?;

    let string_storage = track_section(reader, sections, "string_storage",
        None, |r| Ok(r.read_bytes(header.string_storage_size as usize)?.to_vec()))?;

    Ok(CommonTables {
        function_headers,
        string_kinds,
        identifier_hashes,
        small_string_table,
        overflow_string_table,
        string_storage,
    })
}

// Parse Legacy-specific buffer sections.
fn parse_legacy_buffers(
    reader: &mut ByteReader<'_>,
    header: &BytecodeHeader,
    sections: &mut Vec<SectionInfo>,
) -> Result<LayoutBuffers> {
    let mut big_int_table = Vec::new();
    let mut big_int_storage = Vec::new();
    if let (Some(count), Some(size)) = (header.big_int_count, header.big_int_storage_size) {
        big_int_table = track_section(reader, sections, "bigint_table",
            Some(count), |r| parse_table_entries(r, count))?;
        big_int_storage = track_section(reader, sections, "bigint_storage",
            None, |r| Ok(r.read_bytes(size as usize)?.to_vec()))?;
    }

    let array_buffer = if let Some(size) = header.array_buffer_size {
        track_section(reader, sections, "array_buffer",
            None, |r| Ok(r.read_bytes(size as usize)?.to_vec()))?
    } else {
        Vec::new()
    };

    let obj_key_buffer = track_section(reader, sections, "obj_key_buffer",
        None, |r| Ok(r.read_bytes(header.obj_key_buffer_size as usize)?.to_vec()))?;

    let obj_value_buffer = if let Some(size) = header.obj_value_buffer_size {
        track_section(reader, sections, "obj_value_buffer",
            None, |r| Ok(r.read_bytes(size as usize)?.to_vec()))?
    } else {
        Vec::new()
    };

    Ok(LayoutBuffers {
        big_int_table,
        big_int_storage,
        array_buffer,
        literal_value_buffer: Vec::new(),
        obj_key_buffer,
        obj_value_buffer,
        obj_shape_table: Vec::new(),
    })
}

// Parse Modern-specific buffer sections.
fn parse_modern_buffers(
    reader: &mut ByteReader<'_>,
    header: &BytecodeHeader,
    sections: &mut Vec<SectionInfo>,
) -> Result<LayoutBuffers> {
    let literal_value_buffer = if let Some(size) = header.literal_value_buffer_size {
        track_section(reader, sections, "literal_value_buffer",
            None, |r| Ok(r.read_bytes(size as usize)?.to_vec()))?
    } else {
        Vec::new()
    };

    let obj_key_buffer = track_section(reader, sections, "obj_key_buffer",
        None, |r| Ok(r.read_bytes(header.obj_key_buffer_size as usize)?.to_vec()))?;

    let obj_shape_table = if let Some(count) = header.obj_shape_table_count {
        track_section(reader, sections, "obj_shape_table",
            Some(count), |r| parse_shape_table(r, count))?
    } else {
        Vec::new()
    };

    let mut big_int_table = Vec::new();
    let mut big_int_storage = Vec::new();
    if let (Some(count), Some(size)) = (header.big_int_count, header.big_int_storage_size) {
        big_int_table = track_section(reader, sections, "bigint_table",
            Some(count), |r| parse_table_entries(r, count))?;
        big_int_storage = track_section(reader, sections, "bigint_storage",
            None, |r| Ok(r.read_bytes(size as usize)?.to_vec()))?;
    }

    Ok(LayoutBuffers {
        big_int_table,
        big_int_storage,
        array_buffer: Vec::new(),
        literal_value_buffer,
        obj_key_buffer,
        obj_value_buffer: Vec::new(),
        obj_shape_table,
    })
}

// Parse trailing sections shared by both layouts (regexp, CJS modules, function source,
// instructions) and construct the final BytecodeFile.
fn parse_trailing_and_build(
    bytes: &[u8],
    reader: &mut ByteReader<'_>,
    mut sections: Vec<SectionInfo>,
    header: BytecodeHeader,
    tables: CommonTables,
    buffers: LayoutBuffers,
) -> Result<BytecodeFile> {
    let reg_exp_table = track_section(reader, &mut sections, "regexp_table",
        Some(header.reg_exp_count), |r| parse_table_entries(r, header.reg_exp_count))?;

    let reg_exp_storage = track_section(reader, &mut sections, "regexp_storage",
        None, |r| Ok(r.read_bytes(header.reg_exp_storage_size as usize)?.to_vec()))?;

    let cjs_module_table = track_section(reader, &mut sections, "cjs_module_table",
        Some(header.cjs_module_count), |r| parse_pair_table(r, header.cjs_module_count))?;

    let mut function_source_table = Vec::new();
    if let Some(count) = header.function_source_count {
        function_source_table = track_section(reader, &mut sections, "function_source_table",
            Some(count), |r| parse_pair_table(r, count))?;
    }

    let instruction_offset = reader.position() as u32;
    let instructions = bytes[instruction_offset as usize..].to_vec();

    sections.push(SectionInfo {
        name: "bytecode",
        offset: instruction_offset,
        size: bytes.len() as u32 - instruction_offset,
        entries: None,
    });

    let strings = decode_string_table(
        header.string_count,
        &tables.string_kinds,
        &tables.small_string_table,
        &tables.overflow_string_table,
        &tables.string_storage,
    )?;

    let debug_info = try_parse_debug_info(bytes, header.debug_info_offset);
    let exception_handlers = parse_exception_handlers(bytes, &tables.function_headers);

    Ok(BytecodeFile {
        header,
        function_headers: tables.function_headers,
        string_kinds: tables.string_kinds,
        identifier_hashes: tables.identifier_hashes,
        strings,
        big_int_table: buffers.big_int_table,
        big_int_storage: buffers.big_int_storage,
        reg_exp_table,
        reg_exp_storage,
        array_buffer: buffers.array_buffer,
        literal_value_buffer: buffers.literal_value_buffer,
        obj_key_buffer: buffers.obj_key_buffer,
        obj_value_buffer: buffers.obj_value_buffer,
        obj_shape_table: buffers.obj_shape_table,
        cjs_module_table,
        function_source_table,
        instruction_offset,
        instructions,
        debug_info,
        exception_handlers,
        sections,
    })
}

// Parse exception handler tables for all functions from the raw bytecode bytes.
// Uses `info_offset` from legacy function headers and `hasExceptionHandler` flag (bit 3).
fn parse_exception_handlers(
    bytes: &[u8],
    function_headers: &[FunctionHeader],
) -> BTreeMap<u32, Vec<ExceptionHandler>> {
    let mut result = BTreeMap::new();

    for fh in function_headers {
        let (info_offset, flags, func_id) = match fh {
            FunctionHeader::Legacy(h) => (h.info_offset as usize, h.flags, h.function_id),
            FunctionHeader::Modern(_) => continue, // Modern headers don't have info_offset
        };

        // hasExceptionHandler = bit 3
        if flags & 0x08 == 0 {
            continue;
        }

        if info_offset == 0 || info_offset >= bytes.len() {
            continue;
        }

        // Align to 4 bytes
        let aligned = (info_offset + 3) & !3;
        if aligned + 4 > bytes.len() {
            continue;
        }

        let mut reader = ByteReader::new(&bytes[aligned..]);

        // Read handler count (u32)
        let count = match reader.read_u32() {
            Ok(c) => c,
            Err(_) => continue,
        };

        if count == 0 || count > 1000 {
            // Sanity check — more than 1000 handlers is suspicious
            continue;
        }

        let mut handlers = Vec::with_capacity(count as usize);
        let mut valid = true;

        for _ in 0..count {
            let start = match reader.read_u32() {
                Ok(v) => v,
                Err(_) => { valid = false; break; }
            };
            let end = match reader.read_u32() {
                Ok(v) => v,
                Err(_) => { valid = false; break; }
            };
            let target = match reader.read_u32() {
                Ok(v) => v,
                Err(_) => { valid = false; break; }
            };
            handlers.push(ExceptionHandler { start, end, target });
        }

        if valid && !handlers.is_empty() {
            result.insert(func_id, handlers);
        }
    }

    result
}
