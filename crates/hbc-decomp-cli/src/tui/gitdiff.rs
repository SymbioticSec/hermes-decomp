//! Full-program "git" diff: every function's code laid out side by side, base
//! (file 1) on the left and modified (file 2) on the right, aligned line by
//! line. Built off-thread because it renders all functions of both files.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, OnceLock};
use std::thread;

use hbc_decomp::{BytecodeFile, BytecodeFormat, DecompileOptionsV2};
use regex::Regex;
use similar::{DiffTag, TextDiff};

use super::app::ViewMode;
use super::diff::DiffStatus;
use super::{decompile_or_log, disasm_or_log};

/// Canonicalize build-volatile identifiers so that lines differing only by a
/// Metro module id (e.g. `module_955` vs `module_769`) compare equal and don't
/// show up as real changes. Used only for the *comparison* — the original text
/// is still displayed.
fn normalize(line: &str) -> String {
    static MODULE: OnceLock<Regex> = OnceLock::new();
    static REQUIRE: OnceLock<Regex> = OnceLock::new();
    static DEPMAP: OnceLock<Regex> = OnceLock::new();
    let module = MODULE.get_or_init(|| Regex::new(r"module_\d+").unwrap());
    let require = REQUIRE.get_or_init(|| Regex::new(r"require\(\d+\)").unwrap());
    let depmap = DEPMAP.get_or_init(|| Regex::new(r"dependencyMap\[\d+\]").unwrap());
    let s = module.replace_all(line, "module_#");
    let s = require.replace_all(&s, "require(#)");
    depmap.replace_all(&s, "dependencyMap[#]").into_owned()
}

/// One aligned side-by-side display row.
#[derive(Clone)]
pub enum GitRow {
    /// Function boundary, shown across both columns.
    Header(String),
    /// Unchanged line, present identically on both sides.
    Same {
        old: usize,
        new: usize,
        text: String,
    },
    /// Lines that differ only by volatile ids (e.g. Metro `module_955` vs
    /// `module_769`) — treated as unchanged, shown without highlight.
    Cosmetic {
        old: usize,
        new: usize,
        left: String,
        right: String,
    },
    /// Replaced line: different text on each side.
    Changed {
        old: usize,
        new: usize,
        left: String,
        right: String,
    },
    /// Line only in file 1 (removed).
    Removed { old: usize, text: String },
    /// Line only in file 2 (added).
    Added { new: usize, text: String },
    /// Blank separator between functions.
    Blank,
}

/// Everything the worker needs to render both files. All fields are owned/`Arc`
/// so the job can move to a background thread.
pub struct GitDiffJob {
    pub names: Vec<String>,
    pub map1: HashMap<String, u32>,
    pub map2: HashMap<String, u32>,
    pub diff_status: HashMap<String, DiffStatus>,
    pub file1: Arc<BytecodeFile>,
    pub file2: Arc<BytecodeFile>,
    pub format1: Arc<BytecodeFormat>,
    pub format2: Arc<BytecodeFormat>,
    pub kind: ViewMode,
    /// Ignore volatile Metro ids when deciding what changed.
    pub normalize: bool,
}

/// Resolve a function name to its file-2 id, following renames.
fn id2_for(name: &str, map2: &HashMap<String, u32>, status: &HashMap<String, DiffStatus>) -> Option<u32> {
    if let Some(id) = map2.get(name) {
        return Some(*id);
    }
    if let Some(DiffStatus::Renamed(new_name)) = status.get(name) {
        return map2.get(new_name).copied();
    }
    None
}

fn render(file: &BytecodeFile, format: &BytecodeFormat, id: u32, kind: ViewMode) -> String {
    match kind {
        ViewMode::Disasm => disasm_or_log(file, format, id),
        // Decompile per function and stream it: the user sees progress and
        // content immediately (~0.1s for the first chunk), instead of waiting
        // seconds for a full pipeline build to finish opaquely.
        _ => decompile_or_log(file, format, id, &DecompileOptionsV2::optimized()),
    }
}

/// Align two function bodies into side-by-side rows. When `normalize_ids` is
/// set, the comparison ignores volatile Metro ids (lines differing only by an
/// id become `Cosmetic`, not real changes), but the original text is displayed.
pub fn align(left: &str, right: &str, normalize_ids: bool) -> Vec<GitRow> {
    let old_lines: Vec<&str> = left.lines().collect();
    let new_lines: Vec<&str> = right.lines().collect();
    // Diff on per-line (optionally normalized) slices, NOT a rejoined string:
    // from_slices indices map 1:1 onto old_lines/new_lines, so the op ranges are
    // always valid for display (from_lines can re-tokenize and desync).
    let key = |l: &&str| {
        if normalize_ids {
            normalize(l)
        } else {
            (*l).to_string()
        }
    };
    let old_norm: Vec<String> = old_lines.iter().map(key).collect();
    let new_norm: Vec<String> = new_lines.iter().map(key).collect();
    let old_ref: Vec<&str> = old_norm.iter().map(String::as_str).collect();
    let new_ref: Vec<&str> = new_norm.iter().map(String::as_str).collect();
    let diff = TextDiff::from_slices(&old_ref, &new_ref);

    let mut rows = Vec::new();
    for op in diff.ops() {
        match op.tag() {
            DiffTag::Equal => {
                for (o, n) in op.old_range().zip(op.new_range()) {
                    // Equal under normalization: identical, or differs only by id.
                    if old_lines.get(o).copied().unwrap_or("") == new_lines.get(n).copied().unwrap_or("") {
                        rows.push(GitRow::Same {
                            old: o + 1,
                            new: n + 1,
                            text: old_lines.get(o).copied().unwrap_or("").to_string(),
                        });
                    } else {
                        rows.push(GitRow::Cosmetic {
                            old: o + 1,
                            new: n + 1,
                            left: old_lines.get(o).copied().unwrap_or("").to_string(),
                            right: new_lines.get(n).copied().unwrap_or("").to_string(),
                        });
                    }
                }
            }
            DiffTag::Delete => {
                for o in op.old_range() {
                    rows.push(GitRow::Removed {
                        old: o + 1,
                        text: old_lines.get(o).copied().unwrap_or("").to_string(),
                    });
                }
            }
            DiffTag::Insert => {
                for n in op.new_range() {
                    rows.push(GitRow::Added {
                        new: n + 1,
                        text: new_lines.get(n).copied().unwrap_or("").to_string(),
                    });
                }
            }
            DiffTag::Replace => {
                // Pair left/right lines on the same row; extras spill to
                // Removed/Added so both columns stay aligned.
                let olds: Vec<usize> = op.old_range().collect();
                let news: Vec<usize> = op.new_range().collect();
                for k in 0..olds.len().max(news.len()) {
                    match (olds.get(k), news.get(k)) {
                        (Some(&o), Some(&n)) => rows.push(GitRow::Changed {
                            old: o + 1,
                            new: n + 1,
                            left: old_lines.get(o).copied().unwrap_or("").to_string(),
                            right: new_lines.get(n).copied().unwrap_or("").to_string(),
                        }),
                        (Some(&o), None) => rows.push(GitRow::Removed {
                            old: o + 1,
                            text: old_lines.get(o).copied().unwrap_or("").to_string(),
                        }),
                        (None, Some(&n)) => rows.push(GitRow::Added {
                            new: n + 1,
                            text: new_lines.get(n).copied().unwrap_or("").to_string(),
                        }),
                        (None, None) => {}
                    }
                }
            }
        }
    }
    rows
}

/// Aligned rows for a single function (header + diff + trailing blank).
fn rows_for_function(job: &GitDiffJob, name: &str) -> Vec<GitRow> {
    // An empty side means the function is absent there (added/removed), which is
    // a legitimate diff state — not an error. render() logs real decode errors.
    let left = match job.map1.get(name) {
        Some(&id) => render(&job.file1, &job.format1, id, job.kind),
        None => String::new(),
    };
    let right = match id2_for(name, &job.map2, &job.diff_status) {
        Some(id) => render(&job.file2, &job.format2, id, job.kind),
        None => String::new(),
    };

    let mut rows = vec![GitRow::Header(name.to_string())];
    rows.extend(align(&left, &right, job.normalize));
    rows.push(GitRow::Blank);
    rows
}

/// Whether a row's displayed text contains `query` (which must be lowercase).
pub fn row_contains(row: &GitRow, query: &str) -> bool {
    let has = |s: &str| s.to_lowercase().contains(query);
    match row {
        GitRow::Header(name) => has(name),
        GitRow::Same { text, .. } => has(text),
        GitRow::Cosmetic { left, right, .. } | GitRow::Changed { left, right, .. } => {
            has(left) || has(right)
        }
        GitRow::Removed { text, .. } | GitRow::Added { text, .. } => has(text),
        GitRow::Blank => false,
    }
}

/// Streamed messages from the background git-diff builder. Rows arrive in
/// chunks so the UI can show and scroll partial results while the rest builds.
pub enum GitMsg {
    Chunk {
        rows: Vec<GitRow>,
        done: usize,
        total: usize,
    },
    Done,
}

/// Build the side-by-side rows on a background thread, streaming them in chunks
/// (decompiling 32k functions of both files takes time).
pub fn spawn(job: GitDiffJob) -> Receiver<GitMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let total = job.names.len();
        let mut batch = Vec::new();
        for (i, name) in job.names.iter().enumerate() {
            // Contain any panic to a single function so one bad case can't take
            // down the whole diff build — but log it rather than swallow it.
            let rows = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                rows_for_function(&job, name)
            })) {
                Ok(rows) => rows,
                Err(_) => {
                    super::debug_log(&format!("[gitdiff] panic while rendering '{name}'"));
                    vec![
                        GitRow::Header(name.to_string()),
                        GitRow::Removed {
                            old: 0,
                            text: format!("// <panic while rendering '{name}'>"),
                        },
                        GitRow::Blank,
                    ]
                }
            };
            batch.extend(rows);
            if (i + 1) % 128 == 0 {
                let msg = GitMsg::Chunk {
                    rows: std::mem::take(&mut batch),
                    done: i + 1,
                    total,
                };
                if tx.send(msg).is_err() {
                    return; // UI gone
                }
            }
        }
        let _ = tx.send(GitMsg::Chunk {
            rows: batch,
            done: total,
            total,
        });
        let _ = tx.send(GitMsg::Done);
    });
    rx
}
