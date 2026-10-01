// End-to-end: the built `hermes-decomp` binary against every bytecode file of
// the per-version corpus, each in its own process.
//
// The round-trip script compares outputs, but a decompile that aborts (a
// stack overflow, an out-of-memory kill) only counted as "no decompiled
// output" there and never failed a gate: issue #24 (a JSX props object
// holding an element built from itself) overflowed the stack on every
// version for weeks. Here every bytecode file must decompile in a child
// process, exit 0 within a time budget, produce output, and that output must
// parse under node when node is available. A crash in the child is a failed
// test, not a crashed test harness.
//
// The corpus is generated (`scripts/build/build_corpus.sh`); without it the
// test fails and says so, unless HBC_CORPUS_OPTIONAL is set (CI).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PER_FILE_BUDGET: Duration = Duration::from_secs(60);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

fn corpus_files() -> Vec<PathBuf> {
    let base = repo_root().join("examples/react-native");
    let mut out = Vec::new();
    let Ok(versions) = std::fs::read_dir(&base) else {
        return out;
    };
    for v in versions.flatten() {
        let expressions = v.path().join("expressions");
        let Ok(snippets) = std::fs::read_dir(&expressions) else {
            continue;
        };
        for s in snippets.flatten() {
            let hbc = s.path().join("bytecode.hbc");
            if hbc.is_file() {
                out.push(hbc);
            }
        }
        let metro = v.path().join("metro.hbc");
        if metro.is_file() {
            out.push(metro);
        }
    }
    out.sort();
    out
}

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

// Run the decompiler on one file. `Err` carries the reason.
fn decompile(hbc: &Path, out: &Path) -> Result<(), String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_hermes-decomp"))
        .arg("decompile")
        .arg(hbc)
        .arg("--no-cache")
        .arg("-o")
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut stderr = String::new();
                if let Some(mut s) = child.stderr.take() {
                    use std::io::Read;
                    let _ = s.read_to_string(&mut stderr);
                }
                let tail: String = stderr.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
                return Err(format!("exit {status}: {tail}"));
            }
            Ok(None) if start.elapsed() > PER_FILE_BUDGET => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("no exit within {PER_FILE_BUDGET:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("wait: {e}")),
        }
    }
}

// JSX output is outside node's grammar; the parse check skips it, as the
// repository's syntax check does for whole bundles.
fn looks_like_jsx(js: &Path) -> bool {
    let text = std::fs::read_to_string(js).unwrap_or_default();
    let open = text.contains('<')
        && text.lines().any(|l| {
            l.find('<').is_some_and(|i| {
                l[i + 1..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_uppercase())
            })
        });
    open && (text.contains("/>") || text.contains("</"))
}

fn parses_under_node(js: &Path) -> Result<(), String> {
    if looks_like_jsx(js) {
        return Ok(());
    }
    let output = Command::new("node")
        .arg("--check")
        .arg(js)
        .output()
        .map_err(|e| format!("node: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&output.stderr);
    Err(err.lines().take(3).collect::<Vec<_>>().join(" | "))
}

#[test]
fn every_corpus_file_decompiles_in_its_own_process() {
    let files = corpus_files();
    if files.is_empty() {
        if std::env::var_os("HBC_CORPUS_OPTIONAL").is_some() {
            eprintln!("skipped: no corpus (HBC_CORPUS_OPTIONAL set)");
            return;
        }
        panic!(
            "no corpus under examples/react-native: run scripts/build/fetch_hermesc.sh then \
             scripts/build/build_corpus.sh, or set HBC_CORPUS_OPTIONAL=1 to skip"
        );
    }
    let check_syntax = node_available();
    let tmp = std::env::temp_dir().join(format!("hermes-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("temp dir");
    let mut failures: Vec<String> = Vec::new();
    for (i, hbc) in files.iter().enumerate() {
        let rel = hbc
            .strip_prefix(repo_root())
            .unwrap_or(hbc)
            .display()
            .to_string();
        let out = tmp.join(format!("{i}.js"));
        if let Err(e) = decompile(hbc, &out) {
            failures.push(format!("{rel}: {e}"));
            continue;
        }
        let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        if size == 0 {
            failures.push(format!("{rel}: empty output"));
            continue;
        }
        if check_syntax {
            if let Err(e) = parses_under_node(&out) {
                failures.push(format!("{rel}: output does not parse: {e}"));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        failures.is_empty(),
        "{} of {} corpus files failed:\n  {}",
        failures.len(),
        files.len(),
        failures.join("\n  ")
    );
}

// The crash regressions, as tracked bytecode a few hundred bytes each, so
// they run everywhere, CI included, without a toolchain or a corpus. The
// source of each fixture sits next to it as `<name>.source.js`.
#[test]
fn tracked_regression_fixtures_decompile() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut fixtures: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("tests/fixtures")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "hbc"))
        .collect();
    fixtures.sort();
    assert!(!fixtures.is_empty(), "no fixtures under {}", dir.display());
    let check_syntax = node_available();
    let tmp = std::env::temp_dir().join(format!("hermes-e2e-fix-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("temp dir");
    let mut failures: Vec<String> = Vec::new();
    for hbc in &fixtures {
        let name = hbc.file_name().unwrap().to_string_lossy().into_owned();
        let out = tmp.join(format!("{name}.js"));
        match decompile(hbc, &out) {
            Err(e) => failures.push(format!("{name}: {e}")),
            Ok(()) => {
                let text = std::fs::read_to_string(&out).unwrap_or_default();
                if text.trim().is_empty() {
                    failures.push(format!("{name}: empty output"));
                } else if check_syntax {
                    if let Err(e) = parses_under_node(&out) {
                        failures.push(format!("{name}: output does not parse: {e}"));
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        failures.is_empty(),
        "fixtures failed:\n  {}",
        failures.join("\n  ")
    );
}
