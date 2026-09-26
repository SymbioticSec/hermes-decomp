//! Robustness fuzzer for the Hermes bytecode parser (issue #4 follow-up).
//!
//! The parser must never *panic* on malformed input — it should return an
//! `Err` or a best-effort `BytecodeFile`. This harness mutates real bundles in
//! many ways and asserts `BytecodeFile::parse_auto` never panics:
//!   - truncation at every length,
//!   - version-byte sweep 40..=99 (exercises all version-gated header paths),
//!   - header field zero/max,
//!   - single-byte increments across the header+table region,
//!   - pseudo-random multi-byte flips.
//!
//! Run: cargo run -p hbc-decomp --example fuzz_parse -- <bundle.hbc> [more...]
//! Exits non-zero if any panic is found.

use hbc_decomp::{
    decompile_all_v2_with_closures, decompile_function_v2, BytecodeFile, BytecodeFormat,
    DecompileOptionsV2, FunctionHeaderLayout, HeaderLayout,
};
use std::panic;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// FUZZ_FULL=1 exercises the whole pipeline (closures, IPA, transforms, codegen)
// instead of just per-function decompilation — that's where "elsewhere"
// overflows live. It's much slower, so use it with a small FUZZ_FLIPS.
static FULL: AtomicBool = AtomicBool::new(false);

static PROBES: AtomicUsize = AtomicUsize::new(0);

fn panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

// Run all parse entry points on `bytes`, and — when parsing succeeds — also
// decompile a few functions so the IR builder / codegen path is fuzzed too
// (that's where the switch-table bugs lived). Returns the panic message if any
// path panicked.
fn probe(bytes: &[u8]) -> Option<String> {
    PROBES.fetch_add(1, Ordering::Relaxed);
    let result = panic::catch_unwind(|| {
        // Explicit layouts (the issue mentioned --layout modern).
        let _ = BytecodeFile::parse_with_layout(
            bytes,
            HeaderLayout::Legacy,
            FunctionHeaderLayout::Legacy16,
        );
        let _ = BytecodeFile::parse_with_layout(
            bytes,
            HeaderLayout::Modern,
            FunctionHeaderLayout::Modern12,
        );
        let _ = BytecodeFile::parse_with_layout(
            bytes,
            HeaderLayout::Modern,
            FunctionHeaderLayout::Legacy16,
        );

        if let Ok(file) = BytecodeFile::parse_auto(bytes) {
            if let Ok((format, _)) = BytecodeFormat::for_version_or_latest(file.header.version) {
                let opts = DecompileOptionsV2::default();
                if FULL.load(Ordering::Relaxed) {
                    // Whole-program pipeline: closures, IPA, transforms, codegen.
                    let _ = decompile_all_v2_with_closures(&file, &format, &opts);
                } else {
                    let n = file.function_headers.len().min(8);
                    for id in 0..n {
                        let _ = decompile_function_v2(&file, &format, id as u32, &opts);
                    }
                }
            }
        }
    });
    result.err().map(panic_message)
}

struct Findings {
    panics: Vec<String>,
}

impl Findings {
    fn record(&mut self, strategy: &str, bytes: &[u8]) {
        if let Some(msg) = probe(bytes) {
            let entry = format!("[{strategy}] panic: {msg}");
            if !self.panics.contains(&entry) {
                println!("  !! {entry}");
                self.panics.push(entry);
            }
        }
    }
}

fn fuzz_base(path: &str, f: &mut Findings) {
    let Ok(base) = std::fs::read(path) else {
        println!("  (skip: cannot read {path})");
        return;
    };
    println!("Fuzzing {path} ({} bytes)", base.len());

    // 1) Truncation at every length (dense near the start, sparse later).
    let n = base.len();
    let mut lengths: Vec<usize> = (0..n.min(512)).collect();
    let step = (n / 2000).max(1);
    lengths.extend((0..n).step_by(step));
    lengths.push(n);
    for &len in &lengths {
        f.record("truncate", &base[..len.min(n)]);
    }

    // 2) Version-byte sweep 40..=99 on a full-length copy.
    for v in 40u32..=99 {
        let mut m = base.clone();
        if m.len() >= 12 {
            m[8..12].copy_from_slice(&v.to_le_bytes());
        }
        f.record("version-sweep", &m);
    }

    // 3) Header u32 field zero / max, across the first 512 bytes.
    for off in (0..base.len().min(512)).step_by(4) {
        for fill in [0x00u8, 0xFF] {
            let mut m = base.clone();
            let end = (off + 4).min(m.len());
            for b in &mut m[off..end] {
                *b = fill;
            }
            f.record("field-fill", &m);
        }
    }

    // 4) Single-byte increment across the header + table region.
    for off in 0..base.len().min(2048) {
        let mut m = base.clone();
        m[off] = m[off].wrapping_add(1);
        f.record("byte-incr", &m);
    }

    // 5) Pseudo-random multi-byte flips (deterministic LCG, seeded per file).
    let mut state: u64 = 0x9E3779B97F4A7C15 ^ (base.len() as u64);
    let mut next = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        state >> 33
    };
    let iters: usize = std::env::var("FUZZ_FLIPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5_000);
    for _ in 0..iters {
        let mut m = base.clone();
        let flips = 1 + (next() % 6) as usize;
        for _ in 0..flips {
            if m.is_empty() {
                break;
            }
            let idx = (next() as usize) % m.len();
            m[idx] = (next() & 0xFF) as u8;
        }
        f.record("rand-flip", &m);
    }
}

fn main() {
    // Silence the default panic hook so caught panics don't spam stderr.
    panic::set_hook(Box::new(|_| {}));
    FULL.store(std::env::var("FUZZ_FULL").is_ok(), Ordering::Relaxed);

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: fuzz_parse <bundle.hbc> [more...]");
        std::process::exit(2);
    }

    let mut findings = Findings { panics: Vec::new() };
    for path in &args {
        fuzz_base(path, &mut findings);
    }

    let _ = panic::take_hook();
    println!(
        "\n=== fuzz complete: {} probes, {} distinct panics ===",
        PROBES.load(Ordering::Relaxed),
        findings.panics.len()
    );
    if findings.panics.is_empty() {
        println!("OK: no panics found.");
    } else {
        for p in &findings.panics {
            println!("PANIC: {p}");
        }
        std::process::exit(1);
    }
}
