use crate::error::{Error, Result};
use crate::file::structure::{
    ShapeTableEntry, StringKindEntry, StringKindType, StringTableEntry, TableEntry,
};
use crate::io::ByteReader;

pub fn parse_string_kinds(reader: &mut ByteReader<'_>, count: u32) -> Result<Vec<StringKindEntry>> {
    let mut entries = Vec::with_capacity(reader.capacity_hint(count as usize));
    for _ in 0..count {
        let raw = reader.read_u32()?;
        let kind = if (raw & (1 << 31)) == 0 {
            StringKindType::String
        } else {
            StringKindType::Identifier
        };
        let count = raw & 0x7fff_ffff;
        entries.push(StringKindEntry { kind, count });
    }
    Ok(entries)
}

pub fn parse_overflow_string_table(
    reader: &mut ByteReader<'_>,
    count: u32,
) -> Result<Vec<(u32, u32)>> {
    let mut entries = Vec::with_capacity(reader.capacity_hint(count as usize));
    for _ in 0..count {
        let offset = reader.read_u32()?;
        let length = reader.read_u32()?;
        entries.push((offset, length));
    }
    Ok(entries)
}

pub fn parse_u32_vec(reader: &mut ByteReader<'_>, count: u32) -> Result<Vec<u32>> {
    let mut values = Vec::with_capacity(reader.capacity_hint(count as usize));
    for _ in 0..count {
        values.push(reader.read_u32()?);
    }
    Ok(values)
}

pub fn parse_table_entries(reader: &mut ByteReader<'_>, count: u32) -> Result<Vec<TableEntry>> {
    let mut entries = Vec::with_capacity(reader.capacity_hint(count as usize));
    for _ in 0..count {
        let offset = reader.read_u32()?;
        let length = reader.read_u32()?;
        entries.push(TableEntry { offset, length });
    }
    Ok(entries)
}

pub fn parse_shape_table(reader: &mut ByteReader<'_>, count: u32) -> Result<Vec<ShapeTableEntry>> {
    let mut entries = Vec::with_capacity(reader.capacity_hint(count as usize));
    for _ in 0..count {
        let key_buffer_offset = reader.read_u32()?;
        let num_props = reader.read_u32()?;
        entries.push(ShapeTableEntry {
            key_buffer_offset,
            num_props,
        });
    }
    Ok(entries)
}

pub fn parse_pair_table(reader: &mut ByteReader<'_>, count: u32) -> Result<Vec<(u32, u32)>> {
    let mut entries = Vec::with_capacity(reader.capacity_hint(count as usize));
    for _ in 0..count {
        let first = reader.read_u32()?;
        let second = reader.read_u32()?;
        entries.push((first, second));
    }
    Ok(entries)
}

pub fn decode_string_table(
    string_count: u32,
    kinds: &[StringKindEntry],
    small_entries: &[u32],
    overflow_entries: &[(u32, u32)],
    storage: &[u8],
) -> Result<Vec<StringTableEntry>> {
    // A corrupt or mis-read header can carry a string_count far larger than the
    // actual small-entry table; clamp so both the allocations and the loops
    // below stay bounded (for valid files the two are equal anyway).
    let count = (string_count as usize).min(small_entries.len());
    let mut expanded_kinds = Vec::with_capacity(count);
    let mut kind_index = 0usize;
    let mut remaining = kinds.first().map(|k| k.count).unwrap_or(0);

    for _ in 0..count {
        if remaining == 0 {
            kind_index += 1;
            remaining = kinds.get(kind_index).map(|k| k.count).unwrap_or(0);
        }
        let kind = kinds
            .get(kind_index)
            .map(|k| k.kind)
            .unwrap_or(StringKindType::String);
        expanded_kinds.push(kind);
        remaining = remaining.saturating_sub(1);
    }

    let mut strings = Vec::with_capacity(count);
    let mut overflow_index = 0usize;

    for i in 0..count {
        let raw = small_entries
            .get(i)
            .copied()
            .ok_or_else(|| Error::Parse("string table entry missing".to_string()))?;

        let is_utf16 = (raw & 0x1) != 0;
        let offset = (raw >> 1) & 0x7f_ffff;
        let length = (raw >> 24) & 0xff;

        let (offset, length) = if length == 0xff || offset == 0x800000 {
            let (ov_offset, ov_length) = overflow_entries
                .get(overflow_index)
                .copied()
                .ok_or_else(|| Error::Parse("overflow string entry missing".to_string()))?;
            overflow_index += 1;
            (ov_offset, ov_length)
        } else {
            (offset, length)
        };

        let value = if is_utf16 {
            // saturating math: an overflowing offset/length just fails the
            // bounds check below and yields "<invalid>" instead of panicking.
            let byte_len = (length as usize).saturating_mul(2);
            let start = offset as usize;
            let end = start.saturating_add(byte_len);

            if start >= storage.len() || end > storage.len() {
                "<invalid utf16>".to_string()
            } else {
                let slice = &storage[start..end];
                let mut units = Vec::with_capacity(length as usize);
                for chunk in slice.chunks_exact(2) {
                    units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
                }
                String::from_utf16_lossy(&units)
            }
        } else {
            let start = offset as usize;
            let end = start.saturating_add(length as usize);
            if start >= storage.len() || end > storage.len() {
                "<invalid utf8>".to_string()
            } else {
                String::from_utf8_lossy(&storage[start..end]).to_string()
            }
        };

        let is_identifier = expanded_kinds
            .get(i)
            .copied()
            .unwrap_or(StringKindType::String)
            == StringKindType::Identifier;

        strings.push(StringTableEntry {
            value,
            is_utf16,
            is_identifier,
        });
    }

    Ok(strings)
}
