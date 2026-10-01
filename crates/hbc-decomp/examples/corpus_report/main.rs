//! Per-version corpus validation harness.
//!
//! Walks `examples/react-native/v<N>/`, and for each version:
//!   * parses `bytecode.hbc`, checks the detected HBC version, decodes every
//!     function under `catch_unwind` (parse/decode robustness),
//!   * reads `decompiled.js` and counts quality markers (argN/closure_N/tmp/rN,
//!     decompile-error comments, line count),
//!   * tallies the expression suite (compiled / gap / decompiled),
//!   * folds in `roundtrip.tsv` if present (PASS/FAIL per expression).
//!
//! Writes `v<N>/quality.json` and an aggregate `CORPUS_REPORT.md`.
//!
//! Run: cargo run --release -p hbc-decomp --example corpus_report
//! No-ops gracefully when the (local, gitignored) corpus is absent.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use hbc_decomp::{BytecodeFile, BytecodeFormat};

fn rn_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/react-native")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("examples/react-native"))
}

// Count whole-word tokens like `arg0`, `closure_12`, `tmp`, `r5`.
// `need_digit` requires at least one trailing digit (so `r5` counts but a bare
// `r` in `for` does not).
fn count_token(text: &str, prefix: &str, need_digit: bool) -> usize {
    let bytes = text.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while let Some(pos) = text[i..].find(prefix) {
        let start = i + pos;
        // must be a word start
        let prev_ok = start == 0 || !is_ident_char(bytes[start - 1]);
        let mut j = start + prefix.len();
        let mut digits = 0;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
            digits += 1;
        }
        // token must end at a non-identifier char
        let end_ok = j >= bytes.len() || !is_ident_char(bytes[j]);
        if prev_ok && end_ok && (!need_digit || digits > 0) {
            count += 1;
        }
        i = start + prefix.len();
    }
    count
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

struct VersionReport {
    version: u32,
    detected: Option<u32>,
    parse_ok: bool,
    func_count: u32,
    decode_fail: usize,
    decode_panic: usize,
    lines: usize,
    arg_n: usize,
    closure_n: usize,
    tmp: usize,
    r_n: usize,
    errors: usize,
    expr_total: usize,
    expr_compiled: usize,
    expr_gap: usize,
    rt_pass: usize,
    rt_fail: usize,
    rt_total: usize,
    decompile_ms: u128,
}

fn analyze_version(vdir: &Path, version: u32) -> VersionReport {
    let mut rep = VersionReport {
        version,
        detected: None,
        parse_ok: false,
        func_count: 0,
        decode_fail: 0,
        decode_panic: 0,
        lines: 0,
        arg_n: 0,
        closure_n: 0,
        tmp: 0,
        r_n: 0,
        errors: 0,
        expr_total: 0,
        expr_compiled: 0,
        expr_gap: 0,
        rt_pass: 0,
        rt_fail: 0,
        rt_total: 0,
        decompile_ms: 0,
    };

    // Parse + decode robustness on the canonical app bytecode.
    let hbc = vdir.join("bytecode.hbc");
    if let Ok(bytes) = fs::read(&hbc) {
        if let Ok(file) = BytecodeFile::parse_auto(&bytes) {
            rep.parse_ok = true;
            rep.detected = Some(file.header.version);
            rep.func_count = file.header.function_count;
            if let Ok((format, _)) = BytecodeFormat::for_version_or_latest(file.header.version) {
                let t = Instant::now();
                for id in 0..file.header.function_count {
                    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        file.decode_function_instructions(&format, id)
                    }));
                    match res {
                        Ok(Ok(_)) => {}
                        Ok(Err(_)) => rep.decode_fail += 1,
                        Err(_) => rep.decode_panic += 1,
                    }
                }
                rep.decompile_ms = t.elapsed().as_millis();
            }
        }
    }

    // Quality markers over the produced decompiled.js.
    if let Ok(code) = fs::read_to_string(vdir.join("decompiled.js")) {
        rep.lines = code.lines().count();
        rep.arg_n = count_token(&code, "arg", true);
        rep.closure_n = count_token(&code, "closure_", true);
        rep.tmp = count_token(&code, "tmp", false);
        rep.r_n = count_token(&code, "r", true);
        rep.errors = code.matches("<decompile error").count();
    }

    // Expression suite tally.
    let edir = vdir.join("expressions");
    if let Ok(entries) = fs::read_dir(&edir) {
        for e in entries.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            rep.expr_total += 1;
            if e.path().join("COMPILE_GAP.txt").exists() {
                rep.expr_gap += 1;
            } else if e.path().join("bytecode.hbc").exists() {
                rep.expr_compiled += 1;
            }
        }
    }

    // Round-trip results, if roundtrip.sh has run (name<TAB>STATUS).
    if let Ok(tsv) = fs::read_to_string(vdir.join("roundtrip.tsv")) {
        for line in tsv.lines() {
            let mut it = line.split('\t');
            let _name = it.next();
            match it.next() {
                Some("PASS") => {
                    rep.rt_pass += 1;
                    rep.rt_total += 1;
                }
                Some("FAIL") => {
                    rep.rt_fail += 1;
                    rep.rt_total += 1;
                }
                _ => {}
            }
        }
    }

    rep
}

fn write_quality_json(vdir: &Path, r: &VersionReport) {
    let json = format!(
        "{{\n  \"version\": {},\n  \"detected\": {},\n  \"parse_ok\": {},\n  \"func_count\": {},\n  \"decode_fail\": {},\n  \"decode_panic\": {},\n  \"decompiled_lines\": {},\n  \"argN\": {},\n  \"closure_N\": {},\n  \"tmp\": {},\n  \"rN\": {},\n  \"decompile_errors\": {},\n  \"expr_total\": {},\n  \"expr_compiled\": {},\n  \"expr_gap\": {},\n  \"roundtrip_pass\": {},\n  \"roundtrip_fail\": {},\n  \"roundtrip_total\": {},\n  \"decode_ms\": {}\n}}\n",
        r.version,
        r.detected.map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
        r.parse_ok, r.func_count, r.decode_fail, r.decode_panic, r.lines,
        r.arg_n, r.closure_n, r.tmp, r.r_n, r.errors,
        r.expr_total, r.expr_compiled, r.expr_gap,
        r.rt_pass, r.rt_fail, r.rt_total, r.decompile_ms,
    );
    let _ = fs::write(vdir.join("quality.json"), json);
}

fn main() {
    let rn = rn_dir();
    let mut versions: Vec<(u32, PathBuf)> = Vec::new();
    if let Ok(entries) = fs::read_dir(&rn) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(num) = name.strip_prefix('v').and_then(|s| s.parse::<u32>().ok()) {
                if e.path().join("bytecode.hbc").exists() {
                    versions.push((num, e.path()));
                }
            }
        }
    }
    versions.sort_by_key(|(v, _)| *v);

    if versions.is_empty() {
        println!(
            "No corpus found under {}. Run scripts/build/fetch_hermesc.sh + build_corpus.sh first.",
            rn.display()
        );
        return;
    }

    let mut reports = Vec::new();
    for (v, dir) in &versions {
        let r = analyze_version(dir, *v);
        write_quality_json(dir, &r);
        reports.push(r);
    }

    // Aggregate report.
    let mut md = String::new();
    md.push_str("# Corpus report\n\n");
    md.push_str("Per Hermes bytecode version: parse/decode robustness, decompiled-output quality markers, expression-suite coverage, and A→Z round-trip pass rate. Generated by `cargo run --release -p hbc-decomp --example corpus_report`.\n\n");
    md.push_str("| HBC | parse | detect | funcs | decode fail/panic | dec.lines | argN | closure_N | tmp | rN | dec.err | expr ok/gap | round-trip |\n");
    md.push_str("|----:|:-----:|:------:|------:|:-----------------:|----------:|-----:|----------:|----:|---:|--------:|:-----------:|:----------:|\n");
    for r in &reports {
        let detect = match r.detected {
            Some(d) if d == r.version => "ok".to_string(),
            Some(d) => format!("**{d}**"),
            None => "—".to_string(),
        };
        let rt = if r.rt_total > 0 {
            format!("{}/{}", r.rt_pass, r.rt_total)
        } else {
            "—".to_string()
        };
        md.push_str(&format!(
            "| {} | {} | {} | {} | {}/{} | {} | {} | {} | {} | {} | {} | {}/{} | {} |\n",
            r.version,
            if r.parse_ok { "ok" } else { "FAIL" },
            detect,
            r.func_count,
            r.decode_fail,
            r.decode_panic,
            r.lines,
            r.arg_n,
            r.closure_n,
            r.tmp,
            r.r_n,
            r.errors,
            r.expr_compiled,
            r.expr_gap,
            rt,
        ));
    }
    md.push_str("\nLegend: **detect** bold = detected version differs from the folder/expected version (parser picked wrong layout). `decode panic` > 0 or `dec.err` > 0 are bugs to fix. round-trip `—` means roundtrip.sh hasn't been run for that version.\n");

    let out = rn.join("CORPUS_REPORT.md");
    let _ = fs::write(&out, &md);
    println!("{md}");
    println!("Wrote {}", out.display());
}
