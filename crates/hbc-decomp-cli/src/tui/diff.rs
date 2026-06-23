use hbc_decomp::{BytecodeFile, BytecodeFormat, DecompileOptionsV2};
use std::collections::HashMap;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Instant;

use super::{debug_log, decompile_or_log, disasm_or_log};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffMode {
    Assembly,
    Code,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffStatus {
    Identical,
    Modified,
    Added,
    Removed,
    Renamed(String),
}

#[derive(Debug, Clone)]
pub enum DiffProgressMsg {
    Item {
        name: String,
        status: DiffStatus,
        done: usize,
        total: usize,
    },
    Finished {
        final_status: HashMap<String, DiffStatus>,
    },
}

// Bundles a bytecode file with its format and name→ID map for diff operations.
pub struct DiffFileCtx<'a> {
    pub file: &'a BytecodeFile,
    pub format: &'a BytecodeFormat,
    pub map: &'a HashMap<String, u32>,
}

// Compare two functions and determine their DiffStatus.
//
// This performs a deep comparison:
// 1. Bytecode size check (fast fail)
// 2. Disassembly comparison (stripping offsets)
pub fn compare_functions(
    file1: &BytecodeFile,
    format1: &BytecodeFormat,
    id1: u32,
    file2: &BytecodeFile,
    format2: &BytecodeFormat,
    id2: u32,
    mode: DiffMode,
) -> DiffStatus {
    let h1 = &file1.function_headers[id1 as usize];
    let h2 = &file2.function_headers[id2 as usize];

    // Fast check for Assembly mode
    if mode == DiffMode::Assembly && h1.bytecode_size_in_bytes() != h2.bytecode_size_in_bytes() {
        return DiffStatus::Modified;
    }

    match mode {
        DiffMode::Assembly => {
            // Deep comparison via disassembly
            let dis1 = disasm_or_log(file1, format1, id1);
            let dis2 = disasm_or_log(file2, format2, id2);

            if strip_offsets(&dis1) == strip_offsets(&dis2) {
                DiffStatus::Identical
            } else {
                DiffStatus::Modified
            }
        }
        DiffMode::Code => {
            let options = DecompileOptionsV2::optimized();
            let code1 = decompile_or_log(file1, format1, id1, &options);
            let code2 = decompile_or_log(file2, format2, id2, &options);

            if code1 == code2 {
                DiffStatus::Identical
            } else {
                DiffStatus::Modified
            }
        }
    }
}

fn compute_status_for_name(
    name: &str,
    ctx1: &DiffFileCtx<'_>,
    ctx2: &DiffFileCtx<'_>,
    mode: DiffMode,
) -> DiffStatus {
    let in_1 = ctx1.map.contains_key(name);
    let in_2 = ctx2.map.contains_key(name);

    if in_1 && in_2 {
        let id1 = ctx1.map[name];
        let id2 = ctx2.map[name];
        compare_functions(ctx1.file, ctx1.format, id1, ctx2.file, ctx2.format, id2, mode)
    } else if in_1 {
        DiffStatus::Removed
    } else {
        DiffStatus::Added
    }
}

fn apply_rename_detection(
    diff_status: &mut HashMap<String, DiffStatus>,
    ctx1: &DiffFileCtx<'_>,
    ctx2: &DiffFileCtx<'_>,
    mode: DiffMode,
) {
    let removed_names: Vec<String> = diff_status
        .iter()
        .filter(|(_, s)| matches!(s, DiffStatus::Removed))
        .map(|(n, _)| n.clone())
        .collect();

    let added_names: Vec<String> = diff_status
        .iter()
        .filter(|(_, s)| matches!(s, DiffStatus::Added))
        .map(|(n, _)| n.clone())
        .collect();

    // If we have both removed and added functions, try to match them by content
    if !removed_names.is_empty() && !added_names.is_empty() {
        // Build map of content -> name for Added functions
        let mut added_content: HashMap<String, String> = HashMap::new();

        for name in &added_names {
            let id = ctx2.map[name];
            let content = match mode {
                DiffMode::Assembly => strip_offsets(&disasm_or_log(ctx2.file, ctx2.format, id)),
                DiffMode::Code => {
                    let options = DecompileOptionsV2::optimized();
                    decompile_or_log(ctx2.file, ctx2.format, id, &options)
                }
            };
            if !content.is_empty() {
                added_content.insert(content, name.clone());
            }
        }

        // Check Removed functions
        for name in removed_names {
            let id = ctx1.map[&name];
            let content = match mode {
                DiffMode::Assembly => strip_offsets(&disasm_or_log(ctx1.file, ctx1.format, id)),
                DiffMode::Code => {
                    let options = DecompileOptionsV2::optimized();
                    decompile_or_log(ctx1.file, ctx1.format, id, &options)
                }
            };

            if !content.is_empty() {
                if let Some(new_name) = added_content.get(&content) {
                    // Match found!
                    // Mark old name as Renamed(new_name)
                    diff_status.insert(name.clone(), DiffStatus::Renamed(new_name.clone()));
                    // Remove new name from Added list (hide it? or mark it linked?)
                    // Ideally we remove it from diff_status so it doesn't show up twice
                    diff_status.remove(new_name);
                }
            }
        }
    }
}

pub fn spawn_diff_status_worker(
    file1: Arc<BytecodeFile>,
    format1: Arc<BytecodeFormat>,
    map1: HashMap<String, u32>,
    file2: Arc<BytecodeFile>,
    format2: Arc<BytecodeFormat>,
    map2: HashMap<String, u32>,
    mode: DiffMode,
) -> mpsc::Receiver<DiffProgressMsg> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let started = Instant::now();
        let mut all_names_set = std::collections::HashSet::new();
        for name in map1.keys() {
            all_names_set.insert(name.clone());
        }
        for name in map2.keys() {
            all_names_set.insert(name.clone());
        }

        let mut all_names: Vec<String> = all_names_set.into_iter().collect();
        all_names.sort();
        let total = all_names.len();

        let map1 = Arc::new(map1);
        let map2 = Arc::new(map2);

        let worker_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(1, total.max(1));
        let chunk_size = total.div_ceil(worker_count);
        debug_log(&format!(
            "[TUI] Diff worker started: total={total}, threads={worker_count}, chunk_size={chunk_size}, mode={mode:?}"
        ));

        let (worker_tx, worker_rx) = mpsc::channel::<(String, DiffStatus)>();
        let mut handles = Vec::new();

        for chunk in all_names.chunks(chunk_size.max(1)) {
            let names = chunk.to_vec();
            let tx_chunk = worker_tx.clone();
            let file1 = Arc::clone(&file1);
            let format1 = Arc::clone(&format1);
            let file2 = Arc::clone(&file2);
            let format2 = Arc::clone(&format2);
            let map1 = Arc::clone(&map1);
            let map2 = Arc::clone(&map2);

            handles.push(thread::spawn(move || {
                let c1 = DiffFileCtx { file: &file1, format: &format1, map: &map1 };
                let c2 = DiffFileCtx { file: &file2, format: &format2, map: &map2 };
                for name in names {
                    let status = compute_status_for_name(&name, &c1, &c2, mode);
                    let _ = tx_chunk.send((name, status));
                }
            }));
        }
        drop(worker_tx);

        let mut diff_status = HashMap::new();
        let mut done = 0usize;

        for (name, status) in worker_rx {
            done += 1;
            diff_status.insert(name.clone(), status.clone());
            if done % 500 == 0 || done == total {
                debug_log(&format!("[TUI] Diff progress: {done}/{total}"));
            }
            let _ = tx.send(DiffProgressMsg::Item {
                name,
                status,
                done,
                total,
            });
        }

        for handle in handles {
            let _ = handle.join();
        }

        {
            let c1 = DiffFileCtx { file: &file1, format: &format1, map: &map1 };
            let c2 = DiffFileCtx { file: &file2, format: &format2, map: &map2 };
            apply_rename_detection(&mut diff_status, &c1, &c2, mode);
        }

        let _ = tx.send(DiffProgressMsg::Finished {
            final_status: diff_status,
        });
        debug_log(&format!(
            "[TUI] Diff worker finished in {:.2?} ({} items)",
            started.elapsed(),
            total
        ));
    });

    rx
}

pub fn strip_offsets(s: &str) -> String {
    s.lines()
        .map(|line| {
            if let Some(idx) = line.find(':') {
                if idx < 10 {
                    // assumed offset column
                    return &line[idx + 1..];
                }
            }
            line
        })
        .collect::<String>()
}
