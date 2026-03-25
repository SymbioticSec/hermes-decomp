use super::buffer::read_buffer_series;
use crate::error::Result;
use crate::file::{BytecodeFile, LiteralValue};

// Get BigInt value at the given index.
// Returns the string representation of the BigInt.
pub fn bigint_at(file: &BytecodeFile, bigint_id: u32) -> Option<String> {
    let entry = file.big_int_table.get(bigint_id as usize)?;
    let start = entry.offset as usize;
    let end = start + entry.length as usize;

    if end > file.big_int_storage.len() {
        return None;
    }

    let bytes = &file.big_int_storage[start..end];
    // BigInt is stored as a sequence of bytes representing the value

    if bytes.is_empty() {
        return Some("0".to_string());
    }

    // Check if it's a small value that fits in i64
    if bytes.len() <= 8 {
        let mut value: i64 = 0;
        let is_negative = bytes.last().map(|b| b & 0x80 != 0).unwrap_or(false);

        for (i, &byte) in bytes.iter().enumerate() {
            value |= (byte as i64) << (i * 8);
        }

        // Sign extend if negative
        if is_negative && bytes.len() < 8 {
            let shift = bytes.len() * 8;
            value |= !0i64 << shift;
        }

        return Some(value.to_string());
    }

    // For larger values, show as hex with comment
    let hex: String = bytes.iter().rev().map(|b| format!("{b:02x}")).collect();
    Some(format!("0x{hex}"))
}

pub fn read_array_buffer_series(
    file: &BytecodeFile,
    offset: u32,
    count: u32,
) -> Result<Vec<LiteralValue>> {
    if !file.array_buffer.is_empty() {
        read_buffer_series(file, &file.array_buffer, offset, count)
    } else {
        read_buffer_series(file, &file.literal_value_buffer, offset, count)
    }
}

pub fn read_key_buffer_series(
    file: &BytecodeFile,
    offset: u32,
    count: u32,
) -> Result<Vec<LiteralValue>> {
    read_buffer_series(file, &file.obj_key_buffer, offset, count)
}

pub fn read_value_buffer_series(
    file: &BytecodeFile,
    offset: u32,
    count: u32,
) -> Result<Vec<LiteralValue>> {
    if !file.obj_value_buffer.is_empty() {
        read_buffer_series(file, &file.obj_value_buffer, offset, count)
    } else {
        read_buffer_series(file, &file.literal_value_buffer, offset, count)
    }
}
