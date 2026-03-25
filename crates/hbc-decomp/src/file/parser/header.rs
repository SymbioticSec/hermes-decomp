use crate::error::{Error, Result};
use crate::format::{BytecodeHeader, FunctionHeaderLayout, HeaderLayout};
use crate::io::ByteReader;

pub const MAGIC: u64 = 0x1F1903C103BC1FC6;
pub const HEADER_SIZE: usize = 128;
pub const LEGACY_BIGINT_MIN_VERSION: u32 = 87;
pub const LEGACY_SEGMENT_ID_MIN_VERSION: u32 = 78;
pub const LEGACY_FUNCTION_SOURCE_MIN_VERSION: u32 = 84;
pub const MODERN_FUNCTION_HEADER_MIN_VERSION: u32 = 97;

pub fn infer_function_header_layout(version: u32) -> FunctionHeaderLayout {
    if version >= MODERN_FUNCTION_HEADER_MIN_VERSION {
        FunctionHeaderLayout::Modern12
    } else {
        FunctionHeaderLayout::Legacy16
    }
}

pub fn peek_version(bytes: &[u8]) -> Result<u32> {
    if bytes.len() < 12 {
        return Err(Error::Parse("file too small".to_string()));
    }
    let mut reader = ByteReader::new(bytes);
    let magic = reader.read_u64()?;
    if magic != MAGIC {
        return Err(Error::Parse("invalid magic header".to_string()));
    }
    reader.read_u32()
}

pub fn parse_legacy_header(
    reader: &mut ByteReader<'_>,
    version: u32,
    magic: u64,
    source_hash: [u8; 20],
) -> Result<BytecodeHeader> {
    let file_length = reader.read_u32()?;
    let global_code_index = reader.read_u32()?;
    let function_count = reader.read_u32()?;
    let string_kind_count = reader.read_u32()?;
    let identifier_count = reader.read_u32()?;
    let string_count = reader.read_u32()?;
    let overflow_string_count = reader.read_u32()?;
    let string_storage_size = reader.read_u32()?;

    let (big_int_count, big_int_storage_size) = if version >= LEGACY_BIGINT_MIN_VERSION {
        (Some(reader.read_u32()?), Some(reader.read_u32()?))
    } else {
        (None, None)
    };

    let reg_exp_count = reader.read_u32()?;
    let reg_exp_storage_size = reader.read_u32()?;
    let array_buffer_size = reader.read_u32()?;
    let obj_key_buffer_size = reader.read_u32()?;
    let obj_value_buffer_size = reader.read_u32()?;

    let (segment_id, cjs_module_offset) = if version >= LEGACY_SEGMENT_ID_MIN_VERSION {
        (Some(reader.read_u32()?), None)
    } else {
        (None, Some(reader.read_u32()?))
    };

    let cjs_module_count = reader.read_u32()?;

    let function_source_count = if version >= LEGACY_FUNCTION_SOURCE_MIN_VERSION {
        Some(reader.read_u32()?)
    } else {
        None
    };

    let debug_info_offset = reader.read_u32()?;
    let options = reader.read_u8()?;

    Ok(BytecodeHeader {
        magic,
        version,
        source_hash,
        file_length,
        global_code_index,
        function_count,
        string_kind_count,
        identifier_count,
        string_count,
        overflow_string_count,
        string_storage_size,
        big_int_count,
        big_int_storage_size,
        reg_exp_count,
        reg_exp_storage_size,
        literal_value_buffer_size: None,
        array_buffer_size: Some(array_buffer_size),
        obj_key_buffer_size,
        obj_value_buffer_size: Some(obj_value_buffer_size),
        obj_shape_table_count: None,
        num_string_switch_imms: None,
        segment_id,
        cjs_module_offset,
        cjs_module_count,
        function_source_count,
        debug_info_offset,
        options,
        layout: HeaderLayout::Legacy,
        function_header_layout: infer_function_header_layout(version),
    })
}

pub fn parse_modern_header(
    reader: &mut ByteReader<'_>,
    version: u32,
    magic: u64,
    source_hash: [u8; 20],
) -> Result<BytecodeHeader> {
    let file_length = reader.read_u32()?;
    let global_code_index = reader.read_u32()?;
    let function_count = reader.read_u32()?;
    let string_kind_count = reader.read_u32()?;
    let identifier_count = reader.read_u32()?;
    let string_count = reader.read_u32()?;
    let overflow_string_count = reader.read_u32()?;
    let string_storage_size = reader.read_u32()?;
    let big_int_count = reader.read_u32()?;
    let big_int_storage_size = reader.read_u32()?;
    let reg_exp_count = reader.read_u32()?;
    let reg_exp_storage_size = reader.read_u32()?;
    let literal_value_buffer_size = reader.read_u32()?;
    let obj_key_buffer_size = reader.read_u32()?;
    let obj_shape_table_count = reader.read_u32()?;
    let num_string_switch_imms = reader.read_u32()?;
    let segment_id = reader.read_u32()?;
    let cjs_module_count = reader.read_u32()?;
    let function_source_count = reader.read_u32()?;
    let debug_info_offset = reader.read_u32()?;
    let options = reader.read_u8()?;

    Ok(BytecodeHeader {
        magic,
        version,
        source_hash,
        file_length,
        global_code_index,
        function_count,
        string_kind_count,
        identifier_count,
        string_count,
        overflow_string_count,
        string_storage_size,
        big_int_count: Some(big_int_count),
        big_int_storage_size: Some(big_int_storage_size),
        reg_exp_count,
        reg_exp_storage_size,
        literal_value_buffer_size: Some(literal_value_buffer_size),
        array_buffer_size: None,
        obj_key_buffer_size,
        obj_value_buffer_size: None,
        obj_shape_table_count: Some(obj_shape_table_count),
        num_string_switch_imms: Some(num_string_switch_imms),
        segment_id: Some(segment_id),
        cjs_module_offset: None,
        cjs_module_count,
        function_source_count: Some(function_source_count),
        debug_info_offset,
        options,
        layout: HeaderLayout::Modern,
        function_header_layout: infer_function_header_layout(version),
    })
}
