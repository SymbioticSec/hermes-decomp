//! Regression repro for issue #4: `capacity overflow` panic on Hermes v96 bundles.
//!
//! `hermes-decomp info` / `bin-diff` crashed (exit 101) on real v96 bundles
//! because `DebugInfo::parse` mis-read the debug-info offset and then fed an
//! untrusted `name_count` straight into `Vec::with_capacity`, panicking with
//! "capacity overflow". The panic was raised *inside* the layout-probing done
//! by `parse_auto`, which uses `.ok()` — and `.ok()` cannot catch a panic, so
//! the whole process died instead of falling back.
//!
//! We don't have a redistributable v96 bundle, so this builds a minimal but
//! structurally valid synthetic v96 file whose debug section reproduces the
//! exact trigger, then runs it through the real public entry point
//! (`BytecodeFile::parse_auto`, the same call `info`/`bin-diff` use).
//!
//!   Before the fix: this panics with "capacity overflow" (exit 101).
//!   After the fix:  this parses gracefully (debug info degrades to empty).
//!
//! Run: cargo run -p hbc-decomp --example v96_debug_overflow

use hbc_decomp::BytecodeFile;

const MAGIC: u64 = 0x1F1903C103BC1FC6;
const HEADER_SIZE: usize = 128;
const DEBUG_OFFSET: u32 = HEADER_SIZE as u32;

// Build a minimal v96 `.hbc`: valid magic/header with all section counts zero,
// plus a debug section whose first scope descriptor declares `name_count = -1`
// (sleb128 `0x7F`). `-1 as usize` is `usize::MAX`, the value that used to blow
// up `Vec::with_capacity`.
fn build_synthetic_v96() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&MAGIC.to_le_bytes()); // magic
    b.extend_from_slice(&96u32.to_le_bytes()); // version 96
    b.extend_from_slice(&[0u8; 20]); // source hash

    // Legacy header fields for v96 (every count/size 0 so all sections empty),
    // in the exact order parse_legacy_header reads them.
    let zeros_before_debug = [
        0u32, // file_length
        0,    // global_code_index
        0,    // function_count
        0,    // string_kind_count
        0,    // identifier_count
        0,    // string_count
        0,    // overflow_string_count
        0,    // string_storage_size
        0,    // big_int_count        (v >= 87)
        0,    // big_int_storage_size (v >= 87)
        0,    // reg_exp_count
        0,    // reg_exp_storage_size
        0,    // array_buffer_size
        0,    // obj_key_buffer_size
        0,    // obj_value_buffer_size
        0,    // segment_id           (v >= 78)
        0,    // cjs_module_count
        0,    // function_source_count (v >= 84)
    ];
    for v in zeros_before_debug {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&DEBUG_OFFSET.to_le_bytes()); // debug_info_offset
    b.push(0); // options (u8)

    // Pad to the fixed 128-byte header.
    b.resize(HEADER_SIZE, 0);

    // Debug section at DEBUG_OFFSET: the real 7-field Hermes DebugInfoHeader.
    // The scope-descriptor region holds a poison `name_count` of -1 (sleb128
    // 0x7F). Before the fix this reached `Vec::with_capacity((-1) as usize)` and
    // aborted with "capacity overflow"; now it degrades to empty debug info.
    for v in [
        0u32, // filename_count
        0,    // filename_storage_size
        0,    // file_region_count
        0,    // scope_desc_offset      (data-relative)
        8,    // textified_callee_offset
        8,    // string_table_offset
        8,    // debug_data_size
    ] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    // Debug data (8 bytes). Scope region = [0..8):
    //   parent sleb128 0x7F -> -1, flags 0x00, name_count 0x7F -> -1 (poison).
    b.extend_from_slice(&[0x7F, 0x00, 0x7F, 0x00, 0x00, 0x00, 0x00, 0x00]);
    b
}

fn main() {
    let bytes = build_synthetic_v96();
    println!(
        "synthetic v96 bundle: {} bytes, debug_info_offset = {}",
        bytes.len(),
        DEBUG_OFFSET
    );

    // Same entry point as `info` / `bin-diff`. Pre-fix this panics here.
    match BytecodeFile::parse_auto(&bytes) {
        Ok(file) => {
            println!(
                "parsed without panic: version={}, functions={}, scope_descriptors={}",
                file.header.version,
                file.function_headers.len(),
                file.debug_info.as_ref().map_or(0, |d| d.scope_descriptors.len()),
            );
            println!("OK: issue #4 trigger handled gracefully.");
        }
        Err(e) => {
            // A clean Err is also acceptable: the point is "no panic".
            println!("parsed to a clean error (no panic): {e}");
            println!("OK: issue #4 trigger handled gracefully.");
        }
    }
}
