// Parses the debug info section to extract:
// - Source locations (line/column mappings)
// - Scope descriptors (variable names and scope chain)
// - Textified callees (function call target names)

use crate::error::Result;
use crate::io::ByteReader;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct DebugInfo {
    pub source_locations: BTreeMap<u32, Vec<SourceLocation>>,
    pub scope_descriptors: Vec<ScopeDescriptor>,
    pub textified_callees: BTreeMap<u32, String>,
    pub string_table: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SourceLocation {
    pub bytecode_offset: u32,
    pub line: u32,
    pub column: u32,
    pub scope_offset: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ScopeDescriptor {
    pub offset: u32,
    pub parent_offset: Option<u32>,
    pub flags: u32,
    pub names: Vec<String>,
}

impl ScopeDescriptor {
    pub fn is_inner_scope(&self) -> bool {
        self.flags & 1 != 0
    }

    pub fn is_dynamic(&self) -> bool {
        self.flags & 2 != 0
    }
}

#[derive(Debug, Clone)]
struct DebugInfoOffsets {
    scope_desc_offset: u32,
    textified_callee_offset: u32,
    string_table_offset: u32,
}

impl DebugInfo {
    pub fn parse(bytes: &[u8], debug_info_offset: u32) -> Result<Self> {
        if debug_info_offset == 0 || debug_info_offset == u32::MAX {
            return Ok(Self::default());
        }

        let offset = debug_info_offset as usize;
        if offset >= bytes.len() {
            return Ok(Self::default());
        }

        let mut reader = ByteReader::new(&bytes[offset..]);

        let offsets = Self::parse_header(&mut reader)?;

        let mut debug_info = DebugInfo::default();
        if offsets.scope_desc_offset < offsets.textified_callee_offset {
            let scope_start = offsets.scope_desc_offset as usize;
            let scope_end = offsets.textified_callee_offset as usize;
            if scope_start < bytes.len() - offset && scope_end <= bytes.len() - offset {
                let scope_data = &bytes[offset + scope_start..offset + scope_end];
                debug_info.scope_descriptors = Self::parse_scope_descriptors(scope_data)?;
            }
        }

        if offsets.textified_callee_offset < offsets.string_table_offset {
            let callee_start = offsets.textified_callee_offset as usize;
            let callee_end = offsets.string_table_offset as usize;
            if callee_start < bytes.len() - offset && callee_end <= bytes.len() - offset {
                let callee_data = &bytes[offset + callee_start..offset + callee_end];
                debug_info.textified_callees = Self::parse_textified_callees(callee_data)?;
            }
        }

        if offsets.string_table_offset < (bytes.len() - offset) as u32 {
            let table_start = offsets.string_table_offset as usize;
            if table_start < bytes.len() - offset {
                let table_data = &bytes[offset + table_start..];
                debug_info.string_table = Self::parse_string_table(table_data)?;
            }
        }

        Ok(debug_info)
    }

    fn parse_header(reader: &mut ByteReader<'_>) -> Result<DebugInfoOffsets> {
        let scope_desc_offset = reader.read_u32()?;
        let textified_callee_offset = reader.read_u32()?;
        let string_table_offset = reader.read_u32()?;

        Ok(DebugInfoOffsets {
            scope_desc_offset,
            textified_callee_offset,
            string_table_offset,
        })
    }

    fn parse_scope_descriptors(data: &[u8]) -> Result<Vec<ScopeDescriptor>> {
        let mut descriptors = Vec::new();
        let mut reader = ByteReader::new(data);
        let mut current_offset = 0u32;

        while reader.remaining() > 0 {
            let start_pos = reader.position();

            let parent_raw = reader.read_sleb128()?;
            let parent_offset = if parent_raw < 0 {
                None
            } else {
                Some(parent_raw as u32)
            };

            let flags = reader.read_sleb128()? as u32;

            let name_count = reader.read_sleb128()? as usize;

            let mut names = Vec::with_capacity(name_count);
            for _ in 0..name_count {
                let name = reader.read_length_prefixed_string()?;
                names.push(name);
            }

            descriptors.push(ScopeDescriptor {
                offset: current_offset,
                parent_offset,
                flags,
                names,
            });

            current_offset += (reader.position() - start_pos) as u32;
        }

        Ok(descriptors)
    }

    fn parse_textified_callees(data: &[u8]) -> Result<BTreeMap<u32, String>> {
        let mut callees = BTreeMap::new();
        let mut reader = ByteReader::new(data);

        if reader.remaining() == 0 {
            return Ok(callees);
        }

        let count = reader.read_sleb128()? as usize;

        for _ in 0..count {
            if reader.remaining() == 0 {
                break;
            }
            let address = reader.read_sleb128()? as u32;
            let name = reader.read_length_prefixed_string()?;
            callees.insert(address, name);
        }

        Ok(callees)
    }

    fn parse_string_table(data: &[u8]) -> Result<Vec<String>> {
        let mut strings = Vec::new();
        let mut reader = ByteReader::new(data);

        if reader.remaining() == 0 {
            return Ok(strings);
        }

        let count = match reader.read_sleb128() {
            Ok(c) if c >= 0 => c as usize,
            _ => return Ok(strings),
        };

        for _ in 0..count {
            if reader.remaining() == 0 {
                break;
            }
            match reader.read_length_prefixed_string() {
                Ok(s) => strings.push(s),
                Err(_) => break,
            }
        }

        Ok(strings)
    }

    pub fn build_variable_map(&self, function_scope_offset: Option<u32>) -> BTreeMap<u32, String> {
        let mut var_map = BTreeMap::new();

        if let Some(scope_offset) = function_scope_offset {
            if let Some(scope) = self
                .scope_descriptors
                .iter()
                .find(|s| s.offset == scope_offset)
            {
                for (i, name) in scope.names.iter().enumerate() {
                    if !name.is_empty() {
                        var_map.insert(i as u32, name.clone());
                    }
                }
            }
        }

        var_map
    }

    pub fn all_variable_names(&self) -> Vec<&str> {
        self.scope_descriptors
            .iter()
            .flat_map(|s| s.names.iter().map(|n| n.as_str()))
            .filter(|n| !n.is_empty())
            .collect()
    }
}

pub fn try_parse_debug_info(bytes: &[u8], debug_info_offset: u32) -> Option<DebugInfo> {
    DebugInfo::parse(bytes, debug_info_offset).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_debug_info() {
        let info = DebugInfo::parse(&[], 0).unwrap();
        assert!(info.scope_descriptors.is_empty());
        assert!(info.textified_callees.is_empty());
    }

    #[test]
    fn test_invalid_offset() {
        let info = DebugInfo::parse(&[0u8; 100], u32::MAX).unwrap();
        assert!(info.scope_descriptors.is_empty());
    }
}
