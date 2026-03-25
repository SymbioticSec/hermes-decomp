use crate::error::{Error, Result};

pub struct ByteReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub fn seek(&mut self, pos: usize) -> Result<()> {
        if pos > self.data.len() {
            return Err(Error::Parse(format!(
                "seek out of range: {pos} > {}",
                self.data.len()
            )));
        }
        self.pos = pos;
        Ok(())
    }

    pub fn read_u8(&mut self) -> Result<u8> {
        self.read_exact(1).map(|b| b[0])
    }

    pub fn read_i8(&mut self) -> Result<i8> {
        self.read_u8().map(|v| v as i8)
    }

    pub fn read_u16(&mut self) -> Result<u16> {
        let bytes = self.read_exact(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub fn read_u32(&mut self) -> Result<u32> {
        let bytes = self.read_exact(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn read_u64(&mut self) -> Result<u64> {
        let bytes = self.read_exact(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    pub fn read_i32(&mut self) -> Result<i32> {
        let bytes = self.read_exact(4)?;
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn read_f64(&mut self) -> Result<f64> {
        let bytes = self.read_exact(8)?;
        Ok(f64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    pub fn read_bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        self.read_exact(len)
    }

    pub fn align(&mut self, alignment: usize) -> Result<()> {
        if alignment == 0 {
            return Err(Error::Parse("alignment must be > 0".to_string()));
        }
        let rem = self.pos % alignment;
        if rem == 0 {
            return Ok(());
        }
        let next = self.pos + (alignment - rem);
        self.seek(next)
    }

    fn read_exact(&mut self, len: usize) -> Result<&'a [u8]> {
        if self.pos + len > self.data.len() {
            return Err(Error::Parse(format!(
                "unexpected end of file at {} (needed {len} bytes)",
                self.pos
            )));
        }
        let start = self.pos;
        self.pos += len;
        Ok(&self.data[start..start + len])
    }

    pub fn read_uleb128(&mut self) -> Result<u64> {
        let mut result: u64 = 0;
        let mut shift = 0;
        loop {
            if self.pos >= self.data.len() {
                return Err(Error::Parse("unexpected end of LEB128".to_string()));
            }
            let byte = self.data[self.pos];
            self.pos += 1;
            result |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift >= 64 {
                return Err(Error::Parse("LEB128 overflow".to_string()));
            }
        }
        Ok(result)
    }

    pub fn read_sleb128(&mut self) -> Result<i64> {
        let mut result: i64 = 0;
        let mut shift = 0;
        let mut byte;
        loop {
            if self.pos >= self.data.len() {
                return Err(Error::Parse("unexpected end of SLEB128".to_string()));
            }
            byte = self.data[self.pos];
            self.pos += 1;
            result |= ((byte & 0x7f) as i64) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                break;
            }
            if shift >= 64 {
                return Err(Error::Parse("SLEB128 overflow".to_string()));
            }
        }
        if shift < 64 && (byte & 0x40) != 0 {
            result |= !0i64 << shift;
        }
        Ok(result)
    }

    pub fn read_cstring(&mut self) -> Result<String> {
        let start = self.pos;
        while self.pos < self.data.len() && self.data[self.pos] != 0 {
            self.pos += 1;
        }
        if self.pos >= self.data.len() {
            return Err(Error::Parse("unterminated string".to_string()));
        }
        let bytes = &self.data[start..self.pos];
        self.pos += 1;
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }

    pub fn read_length_prefixed_string(&mut self) -> Result<String> {
        let len = self.read_uleb128()? as usize;
        if len == 0 {
            return Ok(String::new());
        }
        let bytes = self.read_exact(len)?;
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }
}
