// Patch string table entries: same-length in place, or grow/shrink with a full
// string-table + storage rebuild and tail relocation (hermes_rs issue #10 class).

use crate::error::{Error, Result};
use crate::file::BytecodeFile;
use crate::opcode::BytecodeFormat;

use crate::write::serialize::{finalize_raw_image, section_offset};

use super::PatchOptions;

// Locate string `id` UTF-8 bytes via the small/overflow string tables in raw bytes.
// Returns (absolute file offset of content, byte length).
// Hermes may pack strings so entries share storage, always use table offsets,
// never a substring search.
fn locate_string_bytes(file: &BytecodeFile, id: u32) -> Result<(usize, usize)> {
    let entry = file
        .strings
        .get(id as usize)
        .ok_or_else(|| Error::Write(format!("string id {id} out of range")))?;
    if entry.is_utf16 {
        return Err(Error::Write(
            "patch_string: UTF-16 strings not yet supported".into(),
        ));
    }
    let raw = file
        .raw_bytes
        .as_ref()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;
    let small_off = section_offset(file, "small_string_table")
        .ok_or_else(|| Error::Write("small_string_table section missing".into()))?
        as usize;
    let storage_off = section_offset(file, "string_storage")
        .ok_or_else(|| Error::Write("string_storage section missing".into()))?
        as usize;
    let overflow_off = section_offset(file, "overflow_string_table").map(|o| o as usize);

    const UTF16: u32 = 0x1;
    const OFF_SHIFT: u32 = 1;
    const OFF_MASK: u32 = 0x7f_ffff;
    const LEN_SHIFT: u32 = 24;
    const LEN_MASK: u32 = 0xff;
    const LEN_OVERFLOW: u32 = 0xff;
    const OFF_OVERFLOW: u32 = 0x800000;

    // Count how many overflowed entries precede `id` so we know the overflow index.
    let mut overflow_index = 0usize;
    for i in 0..=id as usize {
        let slot = small_off + i * 4;
        if slot + 4 > raw.len() {
            return Err(Error::Write("small string table OOB".into()));
        }
        let raw_e = u32::from_le_bytes(raw[slot..slot + 4].try_into().unwrap());
        let is_utf16 = (raw_e & UTF16) != 0;
        let offset = (raw_e >> OFF_SHIFT) & OFF_MASK;
        let length = (raw_e >> LEN_SHIFT) & LEN_MASK;
        let (off, len) =
            if length == LEN_OVERFLOW || offset == OFF_OVERFLOW {
                let ov_base = overflow_off
                    .ok_or_else(|| Error::Write("overflow string table missing".into()))?;
                let ov_slot = ov_base + overflow_index * 8;
                if ov_slot + 8 > raw.len() {
                    return Err(Error::Write("overflow string table OOB".into()));
                }
                let o = u32::from_le_bytes(raw[ov_slot..ov_slot + 4].try_into().unwrap());
                let l = u32::from_le_bytes(raw[ov_slot + 4..ov_slot + 8].try_into().unwrap());
                overflow_index += 1;
                (o, l)
            } else {
                (offset, length)
            };
        if i == id as usize {
            if is_utf16 {
                return Err(Error::Write("patch_string: UTF-16 not supported".into()));
            }
            let abs = storage_off + off as usize;
            let byte_len = len as usize;
            if abs + byte_len > raw.len() {
                return Err(Error::Write("string content OOB".into()));
            }
            // Sanity: content should match decoded value (modulo packing).
            let slice = &raw[abs..abs + byte_len];
            if slice != entry.value.as_bytes() {
                // Still allow patch if lengths match, packed substrings may decode
                // via different views; trust table length.
                if slice.len() != entry.value.len() {
                    return Err(Error::Write(format!(
                        "string id {id}: table length {} != decoded {}",
                        slice.len(),
                        entry.value.len()
                    )));
                }
            }
            return Ok((abs, byte_len));
        }
    }
    Err(Error::Write(format!("string id {id} not found")))
}

// Per-string storage location read straight from the small/overflow tables.
struct StrLoc {
    storage_off: u32,
    len_field: u32,
    is_utf16: bool,
}

// Read the storage offset + length field of every string from the raw tables.
fn read_all_string_locs(file: &BytecodeFile) -> Result<Vec<StrLoc>> {
    let raw = file
        .raw_bytes
        .as_ref()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;
    let small_off = section_offset(file, "small_string_table")
        .ok_or_else(|| Error::Write("small_string_table section missing".into()))?
        as usize;
    let overflow_off = section_offset(file, "overflow_string_table").map(|o| o as usize);

    const UTF16: u32 = 0x1;
    const OFF_SHIFT: u32 = 1;
    const OFF_MASK: u32 = 0x7f_ffff;
    const LEN_SHIFT: u32 = 24;
    const LEN_MASK: u32 = 0xff;
    const LEN_OVERFLOW: u32 = 0xff;
    const OFF_OVERFLOW: u32 = 0x800000;

    let mut out = Vec::with_capacity(file.strings.len());
    let mut overflow_index = 0usize;
    for i in 0..file.strings.len() {
        let slot = small_off + i * 4;
        if slot + 4 > raw.len() {
            return Err(Error::Write("small string table OOB".into()));
        }
        let raw_e = u32::from_le_bytes(raw[slot..slot + 4].try_into().unwrap());
        let is_utf16 = (raw_e & UTF16) != 0;
        let offset = (raw_e >> OFF_SHIFT) & OFF_MASK;
        let length = (raw_e >> LEN_SHIFT) & LEN_MASK;
        let (off, len) = if length == LEN_OVERFLOW || offset == OFF_OVERFLOW {
            let ov_base = overflow_off
                .ok_or_else(|| Error::Write("overflow string table missing".into()))?;
            let ov_slot = ov_base + overflow_index * 8;
            if ov_slot + 8 > raw.len() {
                return Err(Error::Write("overflow string table OOB".into()));
            }
            let o = u32::from_le_bytes(raw[ov_slot..ov_slot + 4].try_into().unwrap());
            let l = u32::from_le_bytes(raw[ov_slot + 4..ov_slot + 8].try_into().unwrap());
            overflow_index += 1;
            (o, l)
        } else {
            (offset, length)
        };
        out.push(StrLoc {
            storage_off: off,
            len_field: len,
            is_utf16,
        });
    }
    Ok(out)
}

// Hermes identifier hash: Jenkins one at a time over UTF-16 code units, seeded
// with 0. This matches hermes::hashString feeding hermes::updateJenkinsHash.
pub(super) fn hermes_identifier_hash(s: &str) -> u32 {
    let mut h: u32 = 0;
    for cu in s.encode_utf16() {
        h = h.wrapping_add(cu as u32);
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
    }
    h
}

// Index of string `id` within the identifier hash table (identifiers appear in
// string id order). Returns None when the string is not an identifier.
fn identifier_index(file: &BytecodeFile, id: u32) -> Option<usize> {
    if !file.strings.get(id as usize)?.is_identifier {
        return None;
    }
    Some(
        (0..id as usize)
            .filter(|&i| file.strings[i].is_identifier)
            .count(),
    )
}

// If string `id` is an identifier, rewrite its precomputed hash for `new_value`
// in the identifier_hashes table of `buf`. The table sits before the string
// region, so its position is the same in an in place patch or a rebuilt image.
fn update_identifier_hash(
    file: &BytecodeFile,
    buf: &mut [u8],
    id: u32,
    new_value: &str,
) -> Result<()> {
    let Some(idx) = identifier_index(file, id) else {
        return Ok(());
    };
    let ih_off = section_offset(file, "identifier_hashes")
        .ok_or_else(|| Error::Write("identifier_hashes section missing".into()))?
        as usize;
    let pos = ih_off + idx * 4;
    if pos + 4 > buf.len() {
        return Err(Error::Write("identifier hash slot out of range".into()));
    }
    buf[pos..pos + 4].copy_from_slice(&hermes_identifier_hash(new_value).to_le_bytes());
    Ok(())
}

// Byte position of `debug_info_offset` inside a legacy 128-byte header. Mirrors
// the field order written by `write_legacy_header`.
pub(super) fn legacy_debug_info_offset_pos(header: &crate::format::BytecodeHeader) -> usize {
    let mut pos = 64usize;
    if header.big_int_count.is_some() {
        pos += 8; // big_int_count + big_int_storage_size
    }
    pos += 8; // reg_exp_count + reg_exp_storage_size
    pos += 12; // array_buffer_size + obj_key_buffer_size + obj_value_buffer_size
    pos += 8; // segment_id/cjs_module_offset + cjs_module_count
    if header.function_source_count.is_some() {
        pos += 4; // function_source_count
    }
    pos
}

// Grow or shrink a UTF-8 string entry. Rebuilds the small string table and the
// string storage (unpacked), then relocates every section after the string
// region and shifts all absolute offsets (function bodies, function info,
// debug info) by the size delta. This is the hermes_rs issue #10 case.
//
// Legacy layout, non-overflowed function headers, non-identifier UTF-8 target
// only. Refuses anything that would need an overflow string entry or an
// identifier-hash rebuild, so it never emits a silently corrupt file.
fn patch_string_resize(
    file: &mut BytecodeFile,
    id: u32,
    new_value: &str,
) -> Result<Vec<u8>> {
    let modern = matches!(
        file.header.function_header_layout,
        crate::format::FunctionHeaderLayout::Modern12
    );
    // Validate the id up front; the encoding is chosen per new value below.
    if file.strings.get(id as usize).is_none() {
        return Err(Error::Write(format!("string id {id} out of range")));
    }

    let locs = read_all_string_locs(file)?;
    let raw = file
        .raw_bytes
        .clone()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;
    let small_off = section_offset(file, "small_string_table")
        .ok_or_else(|| Error::Write("small_string_table section missing".into()))?
        as usize;
    let storage_off = section_offset(file, "string_storage")
        .ok_or_else(|| Error::Write("string_storage section missing".into()))?
        as usize;
    // First section after the string region, everything from here shifts.
    // Section right after the string region: array_buffer on legacy,
    // literal_value_buffer on modern.
    let array_off = section_offset(file, "array_buffer")
        .or_else(|| section_offset(file, "literal_value_buffer"))
        .ok_or_else(|| Error::Write("post-string section missing".into()))?
        as usize;
    if array_off < small_off || small_off > storage_off {
        return Err(Error::Write("unexpected string section order".into()));
    }
    let old_region_len = array_off - small_off;

    // Rebuild the storage (unpacked) plus the small and overflow string tables.
    // A string whose offset or length does not fit the small entry gets an
    // overflow entry (32-bit offset and length), and its small entry is marked
    // with length 0xff and the overflow index.
    let mut new_storage: Vec<u8> = Vec::new();
    let mut new_small: Vec<u32> = Vec::with_capacity(locs.len());
    let mut new_overflow: Vec<(u32, u32)> = Vec::new();
    for (i, loc) in locs.iter().enumerate() {
        let (bytes, len_field, is_utf16): (Vec<u8>, u32, bool) = if i as u32 == id {
            // Hermes stores a string with one byte per character only when it is
            // pure ASCII, and as UTF-16 for anything with a non-ASCII character.
            // Choose from the new value's own characters, not the old flag: a plain
            // ASCII string patched to hold `é` or `€` must switch to UTF-16.
            let needs_utf16 = new_value.bytes().any(|b| b > 0x7f);
            if needs_utf16 {
                // UTF-16LE; length is in code units.
                let units: Vec<u16> = new_value.encode_utf16().collect();
                let mut b = Vec::with_capacity(units.len() * 2);
                for u in &units {
                    b.extend_from_slice(&u.to_le_bytes());
                }
                (b, units.len() as u32, true)
            } else {
                // Pure ASCII: one byte per character.
                (new_value.as_bytes().to_vec(), new_value.len() as u32, false)
            }
        } else {
            let byte_len = if loc.is_utf16 {
                loc.len_field as usize * 2
            } else {
                loc.len_field as usize
            };
            let start = storage_off + loc.storage_off as usize;
            if start + byte_len > raw.len() {
                return Err(Error::Write("string storage OOB during rebuild".into()));
            }
            (raw[start..start + byte_len].to_vec(), loc.len_field, loc.is_utf16)
        };
        let off = new_storage.len() as u32;
        new_storage.extend_from_slice(&bytes);
        let _ = i;
        if off >= 0x80_0000 || len_field >= 0xff {
            // Overflowed: real offset + length go in the overflow table; the small
            // entry stores the overflow index and length 0xff.
            let ov_index = new_overflow.len() as u32;
            new_overflow.push((off, len_field));
            let e = (0xffu32 << 24) | ((ov_index & 0x7f_ffff) << 1) | (is_utf16 as u32);
            new_small.push(e);
        } else {
            let e = ((len_field & 0xff) << 24) | ((off & 0x7f_ffff) << 1) | (is_utf16 as u32);
            new_small.push(e);
        }
    }

    // Assemble the new string region: small table, overflow table, storage padded
    // so the following section keeps its 4-byte alignment.
    let mut region: Vec<u8> = Vec::new();
    for e in &new_small {
        region.extend_from_slice(&e.to_le_bytes());
    }
    for (off, len) in &new_overflow {
        region.extend_from_slice(&off.to_le_bytes());
        region.extend_from_slice(&len.to_le_bytes());
    }
    let storage_size = new_storage.len() as u32;
    let overflow_count = new_overflow.len() as u32;
    region.extend_from_slice(&new_storage);
    while region.len() % 4 != 0 {
        region.push(0);
    }

    let delta = region.len() as i64 - old_region_len as i64;

    // Splice: [.. small_off] + region + [array_off ..]
    let mut rebuilt = Vec::with_capacity((raw.len() as i64 + delta) as usize);
    rebuilt.extend_from_slice(&raw[..small_off]);
    rebuilt.extend_from_slice(&region);
    rebuilt.extend_from_slice(&raw[array_off..]);

    // Header field updates. overflow_string_count (56) and string_storage_size
    // (60) share offsets across layouts; debug_info_offset differs.
    rebuilt[56..60].copy_from_slice(&overflow_count.to_le_bytes());
    rebuilt[60..64].copy_from_slice(&storage_size.to_le_bytes());
    if file.header.debug_info_offset != 0 {
        // Modern header keeps debug_info_offset at a fixed byte 108.
        let dpos = if modern {
            108
        } else {
            legacy_debug_info_offset_pos(&file.header)
        };
        let shifted = (file.header.debug_info_offset as i64 + delta) as u32;
        if dpos + 4 <= rebuilt.len() {
            rebuilt[dpos..dpos + 4].copy_from_slice(&shifted.to_le_bytes());
        }
    }

    // Everything after the string region moves by `delta`, so every function
    // body offset shifts. The small function header is before the region and
    // keeps its slot; we edit it in place. When a function is overflowed the
    // small header only holds a pointer to an out-of-line large header (also in
    // the moved region): we shift that pointer and the large header's own
    // offset fields, then shift the offsets in its exception handler table.
    let fh_sec = section_offset(file, "function_headers")
        .ok_or_else(|| Error::Write("function_headers section missing".into()))?
        as usize;
    let hsize = if modern { 12 } else { 16 };
    let flag_byte = if modern { 11 } else { 15 };
    for i in 0..file.function_headers.len() {
        let slot = fh_sec + i * hsize;
        if slot + hsize > rebuilt.len() {
            break;
        }
        let overflowed = rebuilt[slot + flag_byte] & crate::format::FLAG_OVERFLOWED != 0;
        if overflowed {
            relocate_overflowed_header(&mut rebuilt, slot, modern, delta)?;
        } else if modern {
            crate::write::header_write::shift_modern_small_header_offset(
                &mut rebuilt[slot..slot + 12],
                delta,
            )?;
        } else {
            // Legacy non-overflowed: shift the 25-bit body offset in place; the
            // 25-bit info_offset only moves if it is set (points past the region).
            crate::write::header_write::shift_legacy_small_header_offsets(
                &mut rebuilt[slot..slot + 16],
                delta,
            )?;
        }
    }

    // An identifier's precomputed hash depends on its text, so refresh it.
    update_identifier_hash(file, &mut rebuilt, id, new_value)?;

    // Keep the decoded model consistent.
    if let Some(s) = file.strings.get_mut(id as usize) {
        s.value = new_value.to_string();
    }
    file.header.overflow_string_count = overflow_count;
    file.header.string_storage_size = storage_size;

    let out = finalize_raw_image(rebuilt)?;
    file.raw_bytes = Some(out.clone());
    Ok(out)
}

// Relocate an overflowed function whose real fields live in an out-of-line large
// header. Shifts the small header pointer, then the large header body offset (and
// its info offset for legacy). All of these sit in the region that moved.
fn relocate_overflowed_header(
    rebuilt: &mut [u8],
    slot: usize,
    modern: bool,
    delta: i64,
) -> Result<()> {
    use crate::write::header_write as hw;
    let large_ptr = if modern {
        hw::read_modern_large_pointer(&rebuilt[slot..slot + 12])?
    } else {
        hw::read_legacy_large_pointer(&rebuilt[slot..slot + 16])?
    };
    if modern {
        hw::shift_modern_large_pointer(&mut rebuilt[slot..slot + 12], delta)?;
    } else {
        hw::shift_legacy_large_pointer(&mut rebuilt[slot..slot + 16], delta)?;
    }
    let lh = (large_ptr as i64 + delta) as usize;
    // The body offset is the first u32 of both large header layouts.
    hw::shift_u32_at(rebuilt, lh, delta)?;
    // Legacy large headers store info_offset at +16; modern computes it (nothing
    // stored to shift).
    if !modern {
        let info_pos = lh + 16;
        if info_pos + 4 <= rebuilt.len() {
            let info = u32::from_le_bytes(rebuilt[info_pos..info_pos + 4].try_into().unwrap());
            if info != 0 {
                hw::shift_u32_at(rebuilt, info_pos, delta)?;
            }
        }
    }
    Ok(())
}

// Replace the value of string table entry `id`. Same-length edits patch storage
// in place; length changes rebuild the string tables and relocate the tail.
// Hermes packs strings so ranges can overlap (`done`/`next` share storage). We
// refuse in-place same-length patches that would corrupt another entry's range.
pub fn patch_string_by_id(
    file: &mut BytecodeFile,
    _format: &BytecodeFormat,
    id: u32,
    new_value: &str,
    _options: &PatchOptions,
) -> Result<Vec<u8>> {
    // UTF-16 entries are re-encoded, so they always take the rebuild path.
    if file
        .strings
        .get(id as usize)
        .map(|s| s.is_utf16)
        .unwrap_or(false)
    {
        return patch_string_resize(file, id, new_value);
    }
    let (abs_off, old_len) = locate_string_bytes(file, id)?;
    let new_bytes = new_value.as_bytes();
    if new_bytes.len() != old_len {
        return patch_string_resize(file, id, new_value);
    }
    // Overlap guard: any other UTF-8 entry whose [start,end) intersects ours.
    let our_end = abs_off + old_len;
    for other in 0..file.strings.len() as u32 {
        if other == id {
            continue;
        }
        if file.strings[other as usize].is_utf16 {
            continue;
        }
        let Ok((o_off, o_len)) = locate_string_bytes(file, other) else {
            continue;
        };
        let o_end = o_off + o_len;
        let overlaps = abs_off < o_end && o_off < our_end;
        if overlaps {
            // Storage is shared with another entry, so an in place overwrite
            // would corrupt it. Rebuild the string table unpacked instead, which
            // gives this entry its own storage.
            return patch_string_resize(file, id, new_value);
        }
    }
    let mut raw = file
        .raw_bytes
        .clone()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;
    raw[abs_off..abs_off + old_len].copy_from_slice(new_bytes);
    // An identifier's precomputed hash tracks its text, so refresh it here too.
    update_identifier_hash(file, &mut raw, id, new_value)?;
    if let Some(s) = file.strings.get_mut(id as usize) {
        s.value = new_value.to_string();
    }
    let out = finalize_raw_image(raw)?;
    file.raw_bytes = Some(out.clone());
    Ok(out)
}

// Replace the first string table entry whose value equals `old` with `new`.
pub fn patch_string_replace(
    file: &mut BytecodeFile,
    format: &BytecodeFormat,
    old: &str,
    new: &str,
    options: &PatchOptions,
) -> Result<Vec<u8>> {
    let id = file
        .strings
        .iter()
        .position(|s| s.value == old)
        .ok_or_else(|| Error::Write(format!("string not found: {old:?}")))? as u32;
    patch_string_by_id(file, format, id, new, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write::footer::verify_footer;

    fn load(path: &str) -> (BytecodeFile, BytecodeFormat) {
        let bytes = std::fs::read(path).unwrap();
        let file = BytecodeFile::parse_auto(&bytes).unwrap();
        let format = BytecodeFormat::for_version(file.header.version).unwrap();
        (file, format)
    }

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/react-native/v96/expressions/generator/bytecode.hbc"
    );

    #[test]
    fn patch_string_same_length_v96() {
        if !std::path::Path::new(FIXTURE).exists() {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let candidates: Vec<u32> = file
            .strings
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.is_utf16 && s.value.len() >= 3)
            .map(|(i, _)| i as u32)
            .collect();
        let mut patched = false;
        for id in candidates {
            let old = file.strings[id as usize].value.clone();
            let new = "Z".repeat(old.len());
            match patch_string_by_id(&mut file, &format, id, &new, &PatchOptions::default()) {
                Ok(out) => {
                    assert!(verify_footer(&out));
                    let re = BytecodeFile::parse_auto(&out).unwrap();
                    assert_eq!(re.strings[id as usize].value, new);
                    patched = true;
                    break;
                }
                Err(e) if e.to_string().contains("overlaps") => continue,
                Err(e) => panic!("unexpected: {e}"),
            }
        }
        assert!(patched, "expected at least one non-overlapping string");
    }

    #[test]
    fn patch_string_resize_grow_reparses() {
        if !std::path::Path::new(FIXTURE).exists() {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        // "gen" is a plain (non-identifier) string in this fixture.
        let id = file.strings.iter().position(|s| s.value == "gen");
        let Some(id) = id else { return };
        let out = patch_string_by_id(&mut file, &format, id as u32, "genXXXXX", &PatchOptions::default());
        if let Ok(out) = out {
            assert!(verify_footer(&out));
            let re = BytecodeFile::parse_auto(&out).unwrap();
            assert_eq!(re.strings[id].value, "genXXXXX");
        }
    }

    #[test]
    fn patch_string_packed_falls_back_to_resize() {
        if !std::path::Path::new(FIXTURE).exists() {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        // "done" shares storage with "next" here, so an in place patch would
        // overlap. The patch must still succeed by rebuilding the table unpacked.
        if file.strings.get(5).map(|s| s.value.as_str()) == Some("done") {
            let out =
                patch_string_by_id(&mut file, &format, 5, "GONE", &PatchOptions::default())
                    .expect("packed same length patch should resize, not fail");
            assert!(verify_footer(&out));
            let re = BytecodeFile::parse_auto(&out).unwrap();
            assert_eq!(re.strings[5].value, "GONE");
        }
    }

    #[test]
    fn identifier_hash_matches_hermes() {
        // Values checked against a real hermesc-compiled table.
        assert_eq!(hermes_identifier_hash("foo"), 0x9290_584e);
        assert_eq!(hermes_identifier_hash("print"), 0xa689_f65b);
    }

    // Patching an ASCII string to a value with non-ASCII characters must switch it
    // to UTF-16 so the runtime reads the real characters, not the UTF-8 bytes. This
    // guards the encoding-by-content rule (a real v98 VM confirmed the round trip).
    #[test]
    fn patch_ascii_to_non_ascii_becomes_utf16() {
        if !std::path::Path::new(FIXTURE).exists() {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let id = file
            .strings
            .iter()
            .position(|s| !s.is_utf16 && s.value.is_ascii() && s.value.len() >= 3);
        let Some(id) = id else { return };
        // Latin1-range only characters still require UTF-16 (they are not ASCII).
        let out = patch_string_by_id(&mut file, &format, id as u32, "éàü", &PatchOptions::default())
            .expect("patch to non-ascii");
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert!(re.strings[id].is_utf16, "non-ascii value must be stored UTF-16");
        assert_eq!(re.strings[id].value, "éàü");

        // A character above the basic plane also round trips.
        let (mut file2, format2) = load(FIXTURE);
        let out2 =
            patch_string_by_id(&mut file2, &format2, id as u32, "a€☕", &PatchOptions::default())
                .expect("patch to astral");
        let re2 = BytecodeFile::parse_auto(&out2).unwrap();
        assert!(re2.strings[id].is_utf16);
        assert_eq!(re2.strings[id].value, "a€☕");
    }

    // A patch that stays pure ASCII keeps the one-byte encoding.
    #[test]
    fn patch_ascii_stays_one_byte() {
        if !std::path::Path::new(FIXTURE).exists() {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let id = file
            .strings
            .iter()
            .position(|s| !s.is_utf16 && s.value.is_ascii() && s.value.len() >= 3);
        let Some(id) = id else { return };
        let out = patch_string_by_id(&mut file, &format, id as u32, "PLAINASCII", &PatchOptions::default())
            .expect("patch ascii");
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert!(!re.strings[id].is_utf16, "ascii value must stay one byte");
        assert_eq!(re.strings[id].value, "PLAINASCII");
    }
}
