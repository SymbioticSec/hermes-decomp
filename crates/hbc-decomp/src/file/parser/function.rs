use crate::error::Result;
use crate::format::{
    BytecodeHeader, FunctionHeader, FunctionHeaderLayout, LegacyFunctionHeader,
    ModernFunctionHeader,
};
use crate::io::ByteReader;

pub fn parse_function_headers(
    reader: &mut ByteReader<'_>,
    header: &BytecodeHeader,
) -> Result<Vec<FunctionHeader>> {
    let mut headers = Vec::with_capacity(reader.capacity_hint(header.function_count as usize));
    for function_id in 0..header.function_count {
        let current_pos = reader.position();
        let function_header = match header.function_header_layout {
            // Legacy Header (16 bytes):
            // Used in Hermes bytecode version < 97.
            // Compacts multiple fields into a single u128 for extreme density.
            // fields: [offset, param_count, size, name, info_offset, frame_size, env_size, registers]
            FunctionHeaderLayout::Legacy16 => {
                let raw = reader.read_bytes(16)?;
                let mut bytes = [0u8; 16];
                bytes.copy_from_slice(raw);
                let raw = u128::from_le_bytes(bytes);
                let offset = (raw & ((1u128 << 25) - 1)) as u32;
                let param_count = ((raw >> 25) & ((1u128 << 7) - 1)) as u32;
                let bytecode_size_in_bytes = ((raw >> 32) & ((1u128 << 15) - 1)) as u32;
                let function_name = ((raw >> 47) & ((1u128 << 17) - 1)) as u32;
                let info_offset = ((raw >> 64) & ((1u128 << 25) - 1)) as u32;
                let frame_size = ((raw >> 89) & ((1u128 << 7) - 1)) as u32;
                let environment_size = ((raw >> 96) & 0xff) as u32;
                let highest_read_cache_index = ((raw >> 104) & 0xff) as u32;
                let highest_write_cache_index = ((raw >> 112) & 0xff) as u32;
                let flags = ((raw >> 120) & 0xff) as u8;

                if flags & 0x20 != 0 {
                    let large_offset = ((info_offset as u64) << 16) | (offset as u64);
                    let large_header =
                        parse_large_header_legacy(reader, large_offset as usize, function_id)?;
                    reader.seek(current_pos + 16)?;
                    FunctionHeader::Legacy(large_header)
                } else {
                    FunctionHeader::Legacy(LegacyFunctionHeader {
                        function_id,
                        offset,
                        param_count,
                        bytecode_size_in_bytes,
                        function_name,
                        info_offset,
                        frame_size,
                        environment_size,
                        highest_read_cache_index,
                        highest_write_cache_index,
                        flags,
                    })
                }
            }
            // Modern Header (12 bytes):
            // Used in Hermes bytecode version >= 97 (including v98).
            // Even more compact (12 bytes vs 16 bytes).
            // Re-arranges bitfields for better packing and newer features (e.g., loop_depth, distinct register counts).
            // This is the default for recent React Native versions (0.75+).
            FunctionHeaderLayout::Modern12 => {
                let raw = reader.read_bytes(12)?;
                let mut bytes = [0u8; 16];
                bytes[..12].copy_from_slice(raw);
                let raw = u128::from_le_bytes(bytes);

                let offset = (raw & ((1u128 << 25) - 1)) as u32;
                let param_count = ((raw >> 25) & ((1u128 << 5) - 1)) as u32;
                let loop_depth = ((raw >> 30) & ((1u128 << 2) - 1)) as u32;
                let bytecode_size_in_bytes = ((raw >> 32) & ((1u128 << 14) - 1)) as u32;
                let function_name = ((raw >> 46) & ((1u128 << 8) - 1)) as u32;
                let number_reg_count = ((raw >> 54) & ((1u128 << 5) - 1)) as u32;
                let non_ptr_reg_count = ((raw >> 59) & ((1u128 << 5) - 1)) as u32;
                let frame_size = ((raw >> 64) & 0xff) as u32;
                let read_cache_size = ((raw >> 72) & 0xff) as u8;
                let write_cache_size = ((raw >> 80) & 0x3f) as u8;
                let num_cache_new_object = ((raw >> 86) & 0x1) as u8;
                let private_name_cache_size = ((raw >> 87) & 0x1) as u8;
                let flags = ((raw >> 88) & 0xff) as u8;

                if flags & 0x20 != 0 {
                    let large_offset =
                        ((function_name as u64) << 24) | (offset as u64 & 0x00ff_ffff);
                    let large_header =
                        parse_large_header_modern(reader, large_offset as usize, function_id)?;
                    reader.seek(current_pos + 12)?;
                    FunctionHeader::Modern(large_header)
                } else {
                    FunctionHeader::Modern(ModernFunctionHeader {
                        function_id,
                        offset,
                        param_count,
                        loop_depth,
                        bytecode_size_in_bytes,
                        function_name,
                        number_reg_count,
                        non_ptr_reg_count,
                        frame_size,
                        read_cache_size,
                        write_cache_size,
                        num_cache_new_object,
                        private_name_cache_size,
                        flags,
                    })
                }
            }
        };
        headers.push(function_header);
    }
    Ok(headers)
}

fn parse_large_header_legacy(
    reader: &mut ByteReader<'_>,
    offset: usize,
    function_id: u32,
) -> Result<LegacyFunctionHeader> {
    let current = reader.position();
    reader.seek(offset)?;
    let header = LegacyFunctionHeader {
        function_id,
        offset: reader.read_u32()?,
        param_count: reader.read_u32()?,
        bytecode_size_in_bytes: reader.read_u32()?,
        function_name: reader.read_u32()?,
        info_offset: reader.read_u32()?,
        frame_size: reader.read_u32()?,
        environment_size: reader.read_u32()?,
        highest_read_cache_index: reader.read_u8()? as u32,
        highest_write_cache_index: reader.read_u8()? as u32,
        flags: reader.read_u8()?,
    };
    reader.seek(current)?;
    Ok(header)
}

fn parse_large_header_modern(
    reader: &mut ByteReader<'_>,
    offset: usize,
    function_id: u32,
) -> Result<ModernFunctionHeader> {
    let current = reader.position();
    reader.seek(offset)?;

    let header = ModernFunctionHeader {
        function_id,
        offset: reader.read_u32()?,
        param_count: reader.read_u32()?,
        loop_depth: reader.read_u32()?,
        bytecode_size_in_bytes: reader.read_u32()?,
        function_name: reader.read_u32()?,
        number_reg_count: reader.read_u32()?,
        non_ptr_reg_count: reader.read_u32()?,
        frame_size: reader.read_u32()?,
        read_cache_size: reader.read_u8()?,
        write_cache_size: reader.read_u8()?,
        num_cache_new_object: reader.read_u8()?,
        private_name_cache_size: reader.read_u8()?,
        flags: reader.read_u8()?,
    };

    reader.seek(current)?;
    Ok(header)
}
