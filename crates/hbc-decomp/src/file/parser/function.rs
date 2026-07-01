use crate::error::Result;
use crate::format::{
    BytecodeHeader, FunctionHeader, FunctionHeaderLayout, LegacyFunctionHeader,
    ModernFunctionHeader, FLAG_OVERFLOWED,
};
use crate::io::ByteReader;

// When a function header is marked `FLAG_OVERFLOWED`, the inline header no
// longer holds the real field values; instead it packs the byte offset to the
// out-of-line "large" header. The two layouts pack that offset differently:
//
//   Legacy16: large_offset = (info_offset << 16) | offset
//   Modern12: large_offset = (function_name << 24) | (offset & 0x00ff_ffff)
//
// The shift amounts below name those packings.
const LEGACY_LARGE_OFFSET_SHIFT: u64 = 16;
const MODERN_LARGE_OFFSET_SHIFT: u64 = 24;
// Mask for the low 24 bits of the Modern packed offset (the `offset` portion).
const MODERN_LARGE_OFFSET_MASK: u64 = 0x00ff_ffff;

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
                // Legacy16 bitfield map — (bit offset, width) within the 128-bit word:
                //   offset                    : ( 0, 25)
                //   param_count               : (25,  7)
                //   bytecode_size_in_bytes    : (32, 15)
                //   function_name             : (47, 17)
                //   info_offset               : (64, 25)
                //   frame_size                : (89,  7)
                //   environment_size          : (96,  8)
                //   highest_read_cache_index  : (104, 8)
                //   highest_write_cache_index : (112, 8)
                //   flags                     : (120, 8)
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

                if flags & FLAG_OVERFLOWED != 0 {
                    let large_offset =
                        ((info_offset as u64) << LEGACY_LARGE_OFFSET_SHIFT) | (offset as u64);
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

                // Modern12 bitfield map — (bit offset, width) within the 96-bit word:
                //   offset                  : ( 0, 25)
                //   param_count             : (25,  5)
                //   loop_depth              : (30,  2)
                //   bytecode_size_in_bytes  : (32, 14)
                //   function_name           : (46,  8)
                //   number_reg_count        : (54,  5)
                //   non_ptr_reg_count       : (59,  5)
                //   frame_size              : (64,  8)
                //   read_cache_size         : (72,  8)
                //   write_cache_size        : (80,  6)
                //   num_cache_new_object    : (86,  1)
                //   private_name_cache_size : (87,  1)
                //   flags                   : (88,  8)
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

                if flags & FLAG_OVERFLOWED != 0 {
                    let large_offset = ((function_name as u64) << MODERN_LARGE_OFFSET_SHIFT)
                        | (offset as u64 & MODERN_LARGE_OFFSET_MASK);
                    let large_header =
                        parse_large_header_modern(reader, large_offset as usize, function_id)?;
                    reader.seek(current_pos + 12)?;
                    FunctionHeader::Modern(large_header)
                } else {
                    // Not overflowed: a 12-byte small header has no FunctionInfo
                    // section, so info_offset is 0 (no exception handlers).
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
                        info_offset: 0,
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

    let mut header = ModernFunctionHeader {
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
        info_offset: 0,
    };

    // The FunctionInfo (exception handler table, then debug info) is laid out
    // immediately after the large header, 4-byte aligned. HBC >=97 small headers
    // carry no info_offset field, so a function with exception handlers / debug
    // info is emitted overflowed and its info section is located here.
    let after = reader.position();
    header.info_offset = ((after + 3) & !3) as u32;

    reader.seek(current)?;
    Ok(header)
}
