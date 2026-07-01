// Parses the debug info section to extract:
// - Source locations (line/column mappings)
// - Scope descriptors (variable names and scope chain)
// - Textified callees (function call target names)

use crate::error::Result;
use crate::io::ByteReader;
use std::collections::BTreeMap;

// `data[start..start+len]` if fully in bounds, else `None`. Uses u64 so huge
// header values can't overflow the index arithmetic.
fn slice_in_bounds(data: &[u8], start: u64, len: u64) -> Option<&[u8]> {
    let start = usize::try_from(start).ok()?;
    let len = usize::try_from(len).ok()?;
    let end = start.checked_add(len)?;
    data.get(start..end)
}

// `data[start..end]` if `start <= end <= data.len()`, else `None`.
fn slice_range(data: &[u8], start: u32, end: u32) -> Option<&[u8]> {
    if start > end {
        return None;
    }
    data.get(start as usize..end as usize)
}

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

// Parsed Hermes `DebugInfoHeader` (7 little-endian u32 fields).
//
// The section layout is:
//   [DebugInfoHeader (28 bytes)]
//   [filename table: filename_count * 8 bytes ({offset,length}) + filename_storage_size bytes]
//   [file regions:   file_region_count * 12 bytes]
//   [debug data (debug_data_size bytes):
//       [0 .. scope_desc_offset)             source-location / line data
//       [scope_desc_offset .. callee_offset) scope descriptors
//       [callee_offset .. string_offset)     textified callees
//       [string_offset .. debug_data_size)   debug string table ]
//
// The three offsets are relative to the START OF THE DEBUG DATA, not to the
// section start — a distinction the old 3-field reader got wrong, which is the
// root cause of issue #4 (it read the filename/region counts as offsets).
#[derive(Debug, Clone)]
struct DebugInfoHeader {
    filename_count: u32,
    filename_storage_size: u32,
    file_region_count: u32,
    scope_desc_offset: u32,
    textified_callee_offset: u32,
    string_table_offset: u32,
    debug_data_size: u32,
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
        let header = Self::parse_header(&mut reader)?;

        // Where the debug-data blob begins, relative to the section start.
        // Every term is bounded by header values; use u64 + saturating math so
        // a corrupt header can never overflow or index out of range.
        let data_start = 28u64
            + (header.filename_count as u64).saturating_mul(8)
            + header.filename_storage_size as u64
            + (header.file_region_count as u64).saturating_mul(12);
        let section = &bytes[offset..];
        let Some(data) = slice_in_bounds(section, data_start, header.debug_data_size as u64) else {
            // Header points past the file: treat as "no debug info" rather than
            // failing the whole bytecode parse.
            return Ok(Self::default());
        };

        let mut debug_info = DebugInfo::default();

        // Parse the string table first: scope descriptors and callees refer to
        // their names by index into it.
        if let Some(table) = slice_range(
            data,
            header.string_table_offset,
            header.debug_data_size,
        ) {
            debug_info.string_table = Self::parse_string_table(table);
        }

        if let Some(scope_data) = slice_range(
            data,
            header.scope_desc_offset,
            header.textified_callee_offset,
        ) {
            debug_info.scope_descriptors =
                Self::parse_scope_descriptors(scope_data, &debug_info.string_table);
        }

        if let Some(callee_data) = slice_range(
            data,
            header.textified_callee_offset,
            header.string_table_offset,
        ) {
            debug_info.textified_callees =
                Self::parse_textified_callees(callee_data, &debug_info.string_table);
        }

        Ok(debug_info)
    }

    fn parse_header(reader: &mut ByteReader<'_>) -> Result<DebugInfoHeader> {
        Ok(DebugInfoHeader {
            filename_count: reader.read_u32()?,
            filename_storage_size: reader.read_u32()?,
            file_region_count: reader.read_u32()?,
            scope_desc_offset: reader.read_u32()?,
            textified_callee_offset: reader.read_u32()?,
            string_table_offset: reader.read_u32()?,
            debug_data_size: reader.read_u32()?,
        })
    }

    // Resolve a debug-string-table index to its name, if in range.
    fn name_at(table: &[String], index: i64) -> Option<String> {
        usize::try_from(index)
            .ok()
            .and_then(|i| table.get(i))
            .cloned()
    }

    fn parse_scope_descriptors(data: &[u8], strings: &[String]) -> Vec<ScopeDescriptor> {
        let mut descriptors = Vec::new();
        let mut reader = ByteReader::new(data);
        let mut current_offset = 0u32;

        while reader.remaining() > 0 {
            let start_pos = reader.position();

            // A malformed/mis-aligned section can desync here; bail on the first
            // read error or implausible count rather than panic or loop wildly.
            let Ok(parent_raw) = reader.read_sleb128() else {
                break;
            };
            let Ok(flags) = reader.read_sleb128() else {
                break;
            };
            let Ok(name_count) = reader.read_sleb128() else {
                break;
            };
            if !(0..=reader.remaining() as i64).contains(&name_count) {
                break;
            }

            let mut names = Vec::new();
            for _ in 0..name_count {
                let Ok(name_idx) = reader.read_sleb128() else {
                    break;
                };
                // Out-of-range index => unknown name (empty) rather than wrong.
                names.push(Self::name_at(strings, name_idx).unwrap_or_default());
            }

            // Hermes encodes "no parent" as the u32 sentinel (all ones).
            let parent_offset = if parent_raw < 0 || parent_raw >= u32::MAX as i64 {
                None
            } else {
                Some(parent_raw as u32)
            };

            descriptors.push(ScopeDescriptor {
                offset: current_offset,
                parent_offset,
                flags: flags as u32,
                names,
            });

            current_offset += (reader.position() - start_pos) as u32;
        }

        descriptors
    }

    fn parse_textified_callees(data: &[u8], strings: &[String]) -> BTreeMap<u32, String> {
        let mut callees = BTreeMap::new();
        let mut reader = ByteReader::new(data);

        let Ok(count) = reader.read_sleb128() else {
            return callees;
        };
        if !(0..=reader.remaining() as i64).contains(&count) {
            return callees;
        }

        for _ in 0..count {
            let (Ok(address), Ok(name_idx)) = (reader.read_sleb128(), reader.read_sleb128()) else {
                break;
            };
            if let Some(name) = Self::name_at(strings, name_idx) {
                callees.insert(address as u32, name);
            }
        }

        callees
    }

    // The debug string table is a run of length-prefixed strings filling the
    // region — there is no leading count.
    fn parse_string_table(data: &[u8]) -> Vec<String> {
        let mut strings = Vec::new();
        let mut reader = ByteReader::new(data);
        while reader.remaining() > 0 {
            match reader.read_length_prefixed_string() {
                Ok(s) => strings.push(s),
                Err(_) => break,
            }
        }
        strings
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

    // Build a complete, well-formed Hermes debug section: the 7-field header, a
    // real filename table (`{offset,length}` entries + concatenated storage)
    // and a file-region table, then the debug-data blob (scope descriptors,
    // textified callees, string table). This lays the section out exactly like
    // a real bundle, so it exercises the data-start arithmetic
    // (`28 + 8*filenameCount + filenameStorageSize + 12*fileRegionCount`) —
    // not a degenerate empty-table shortcut. The section offsets are derived
    // from the actual blob sizes. Returns (full buffer, offset for `parse`).
    fn build_debug_section(
        filenames: &[&str],
        file_regions: u32,
        scope_data: &[u8],
        callee_data: &[u8],
        string_table: &[u8],
    ) -> (Vec<u8>, u32) {
        // Filename table: an {offset, length} pair per name, then the storage.
        let mut storage = Vec::new();
        let mut entries = Vec::new();
        for name in filenames {
            entries.push((storage.len() as u32, name.len() as u32));
            storage.extend_from_slice(name.as_bytes());
        }

        // Offsets are relative to the start of the debug-data blob.
        let scope_off = 0u32;
        let callee_off = scope_data.len() as u32;
        let string_off = callee_off + callee_data.len() as u32;
        let data_size = string_off + string_table.len() as u32;

        let mut bytes = vec![0u8; 4]; // prefix so the offset is non-zero
        for v in [
            filenames.len() as u32,
            storage.len() as u32,
            file_regions,
            scope_off,
            callee_off,
            string_off,
            data_size,
        ] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        // Filename table: entries, then storage.
        for (off, len) in &entries {
            bytes.extend_from_slice(&off.to_le_bytes());
            bytes.extend_from_slice(&len.to_le_bytes());
        }
        bytes.extend_from_slice(&storage);
        // File regions: file_regions * 12 bytes (contents irrelevant here).
        bytes.extend(vec![0u8; file_regions as usize * 12]);
        // Debug data: scope descriptors, callees, string table.
        bytes.extend_from_slice(scope_data);
        bytes.extend_from_slice(callee_data);
        bytes.extend_from_slice(string_table);
        (bytes, 4)
    }

    #[test]
    fn test_parses_real_section_with_filenames_and_regions() {
        // One scope: parent=-1 (0x7f), flags=0, name_count=1, name_idx=0 -> "hi".
        let scope = [0x7f, 0x00, 0x01, 0x00];
        let strings = [0x02, b'h', b'i']; // string table: one entry "hi"
        // Two filenames + one file region so data_start =
        // 28 + 8*2 + len("app.js"=6)+len("b.js"=4) + 12*1 = 28+16+10+12 = 66.
        let (bytes, off) = build_debug_section(&["app.js", "b.js"], 1, &scope, &[], &strings);
        let info = DebugInfo::parse(&bytes, off).unwrap();
        assert_eq!(info.string_table, vec!["hi".to_string()]);
        assert_eq!(info.scope_descriptors.len(), 1);
        assert_eq!(info.scope_descriptors[0].names, vec!["hi".to_string()]);
        assert_eq!(info.scope_descriptors[0].parent_offset, None);
    }

    // Regression for issue #4: a mis-located/short debug section (as happens on
    // v96 bundles) used to decode an absurd name count and panic in
    // Vec::with_capacity ("capacity overflow"). Parsing must now degrade to a
    // clean result instead, never panicking, for any input.
    #[test]
    fn test_malformed_debug_info_never_panics() {
        // Poison name_count (-1) in the scope region of an otherwise valid section.
        let scope = [0x7f, 0x00, 0x7f]; // parent=-1, flags=0, name_count=-1
        let (bytes, off) = build_debug_section(&["a.js"], 1, &scope, &[], &[]);
        let info = DebugInfo::parse(&bytes, off).expect("must not panic");
        assert!(info.scope_descriptors.is_empty());

        // Arbitrary garbage offsets / truncated buffers must not panic either.
        for len in [0usize, 1, 8, 28, 40] {
            let junk = vec![0xffu8; len];
            let _ = DebugInfo::parse(&junk, 1);
            let _ = DebugInfo::parse(&junk, len as u32);
        }
    }
}
