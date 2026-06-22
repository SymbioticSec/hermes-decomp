use crate::error::{Error, Result};
use crate::file::structure::{BytecodeFile, LiteralValue};
use crate::io::ByteReader;

#[derive(Debug, Clone, Copy)]
enum DataBufferTag {
    Null,
    True,
    False,
    Number,
    LongString,
    ShortString,
    ByteString,
    Integer,
    Undefined,
}

pub fn read_buffer_series(
    file: &BytecodeFile,
    buffer: &[u8],
    offset: u32,
    count: u32,
) -> Result<Vec<LiteralValue>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if offset as usize >= buffer.len() {
        return Err(Error::Parse(format!(
            "buffer offset out of range: {} >= {}",
            offset,
            buffer.len()
        )));
    }

    let mut reader = ByteReader::new(&buffer[offset as usize..]);
    let mut values = Vec::with_capacity(reader.capacity_hint(count as usize));

    while values.len() < count as usize {
        let (tag, length) = read_buffer_tag(&mut reader)?;
        if length == 0 {
            return Err(Error::Parse("data buffer entry length is zero".to_string()));
        }
        for _ in 0..length {
            let value = read_buffer_value(file, tag, &mut reader)?;
            values.push(value);
            if values.len() == count as usize {
                break;
            }
        }
    }

    Ok(values)
}

fn read_buffer_tag(reader: &mut ByteReader<'_>) -> Result<(DataBufferTag, u32)> {
    let key_tag = reader.read_u8()?;
    let tag_bits = key_tag & 0x70;
    let length = if (key_tag & 0x80) != 0 {
        let next = reader.read_u8()? as u32;
        ((key_tag & 0x0f) as u32) << 8 | next
    } else {
        (key_tag & 0x0f) as u32
    };

    let tag = match tag_bits {
        0x00 => DataBufferTag::Null,
        0x10 => DataBufferTag::True,
        0x20 => DataBufferTag::False,
        0x30 => DataBufferTag::Number,
        0x40 => DataBufferTag::LongString,
        0x50 => DataBufferTag::ShortString,
        0x60 => DataBufferTag::ByteString,
        0x70 => DataBufferTag::Integer,
        _ => DataBufferTag::Undefined,
    };

    Ok((tag, length))
}

fn read_buffer_value(
    file: &BytecodeFile,
    tag: DataBufferTag,
    reader: &mut ByteReader<'_>,
) -> Result<LiteralValue> {
    Ok(match tag {
        DataBufferTag::Null => LiteralValue::Null,
        DataBufferTag::True => LiteralValue::Bool(true),
        DataBufferTag::False => LiteralValue::Bool(false),
        DataBufferTag::Number => LiteralValue::Number(reader.read_f64()?),
        DataBufferTag::Integer => LiteralValue::Integer(reader.read_i32()?),
        DataBufferTag::ShortString => {
            let id = reader.read_u16()? as u32;
            let value = file
                .string_at(id)
                .map(|entry| entry.value.clone())
                .unwrap_or_else(|| format!("<string:{id}>"));
            LiteralValue::String(value)
        }
        DataBufferTag::LongString => {
            let id = reader.read_u32()?;
            let value = file
                .string_at(id)
                .map(|entry| entry.value.clone())
                .unwrap_or_else(|| format!("<string:{id}>"));
            LiteralValue::String(value)
        }
        DataBufferTag::ByteString => {
            let id = reader.read_u8()? as u32;
            let value = file
                .string_at(id)
                .map(|entry| entry.value.clone())
                .unwrap_or_else(|| format!("<string:{id}>"));
            LiteralValue::String(value)
        }
        DataBufferTag::Undefined => LiteralValue::Undefined,
    })
}
