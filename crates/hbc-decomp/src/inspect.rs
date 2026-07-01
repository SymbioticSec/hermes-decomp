//! Analyst helpers shared between the CLI and the MCP server.
//!
//! Three capability groups live here so both front-ends render identical output
//! without duplicating logic:
//!   * structural-table dumps ([`dump_table`] / [`dump_table_json`])
//!   * per-function metadata banners ([`function_info_banner`])
//!   * call-graph rendering ([`render_call_graph`])

use crate::error::Result;
use crate::file::BytecodeFile;
use crate::format::FunctionHeader;
use crate::opcode::BytecodeFormat;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

// The structural tables that can be dumped beyond `strings` / `functions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    CjsModules,
    RegExp,
    ObjShapes,
    FunctionSources,
    StringKinds,
    Sections,
    BigInt,
    ArrayBuffer,
}

impl TableKind {
    // Parse a CLI/MCP table-kind string (accepts both `-` and `_` separators).
    pub fn parse(s: &str) -> Option<Self> {
        match s.replace('_', "-").to_ascii_lowercase().as_str() {
            "cjs-modules" | "cjs" => Some(Self::CjsModules),
            "regexp" | "regex" => Some(Self::RegExp),
            "obj-shapes" | "shapes" => Some(Self::ObjShapes),
            "function-sources" | "func-sources" => Some(Self::FunctionSources),
            "string-kinds" => Some(Self::StringKinds),
            "sections" => Some(Self::Sections),
            "big-int" | "bigint" => Some(Self::BigInt),
            "array-buffer" | "arraybuffer" => Some(Self::ArrayBuffer),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::CjsModules => "cjs-modules",
            Self::RegExp => "regexp",
            Self::ObjShapes => "obj-shapes",
            Self::FunctionSources => "function-sources",
            Self::StringKinds => "string-kinds",
            Self::Sections => "sections",
            Self::BigInt => "big-int",
            Self::ArrayBuffer => "array-buffer",
        }
    }
}

// Render a short hex preview of a byte slice (first `max` bytes).
fn hex_preview(bytes: &[u8], max: usize) -> String {
    let shown = bytes.len().min(max);
    let mut s = bytes[..shown]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    if bytes.len() > shown {
        s.push_str(&format!(" ... (+{} bytes)", bytes.len() - shown));
    }
    s
}

fn slice_from(storage: &[u8], offset: u32, length: u32) -> &[u8] {
    let start = (offset as usize).min(storage.len());
    let end = start.saturating_add(length as usize).min(storage.len());
    &storage[start..end]
}

// Dump a structural table to a human-readable, labeled string.
pub fn dump_table(file: &BytecodeFile, kind: TableKind) -> String {
    let mut out = String::new();
    match kind {
        TableKind::CjsModules => {
            out.push_str(&format!(
                "CommonJS Module Table ({} entries):\n",
                file.cjs_module_table.len()
            ));
            out.push_str("----------------------------------------\n");
            for (i, (symbol_id, function_id)) in file.cjs_module_table.iter().enumerate() {
                out.push_str(&format!(
                    "[{i}] symbol_id={symbol_id} function_id={function_id}\n"
                ));
            }
        }
        TableKind::RegExp => {
            out.push_str(&format!(
                "RegExp Table ({} entries, storage {} bytes):\n",
                file.reg_exp_table.len(),
                file.reg_exp_storage.len()
            ));
            out.push_str("----------------------------------------\n");
            for (i, entry) in file.reg_exp_table.iter().enumerate() {
                let bytes = slice_from(&file.reg_exp_storage, entry.offset, entry.length);
                out.push_str(&format!(
                    "[{i}] offset={} length={} bytecode=[{}]\n",
                    entry.offset,
                    entry.length,
                    hex_preview(bytes, 16)
                ));
            }
        }
        TableKind::ObjShapes => {
            out.push_str(&format!(
                "Object Shape Table ({} entries):\n",
                file.obj_shape_table.len()
            ));
            out.push_str("----------------------------------------\n");
            for (i, entry) in file.obj_shape_table.iter().enumerate() {
                out.push_str(&format!(
                    "[{i}] key_buffer_offset={} num_props={}\n",
                    entry.key_buffer_offset, entry.num_props
                ));
            }
        }
        TableKind::FunctionSources => {
            out.push_str(&format!(
                "Function Source Table ({} entries):\n",
                file.function_source_table.len()
            ));
            out.push_str("----------------------------------------\n");
            for (i, (function_id, string_id)) in file.function_source_table.iter().enumerate() {
                let src = file
                    .string_at(*string_id)
                    .map(|e| crate::escape_js_string(&e.value))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "[{i}] function_id={function_id} string_id={string_id} {src}\n"
                ));
            }
        }
        TableKind::StringKinds => {
            out.push_str(&format!(
                "String Kind Table ({} run-length entries):\n",
                file.string_kinds.len()
            ));
            out.push_str("----------------------------------------\n");
            for (i, entry) in file.string_kinds.iter().enumerate() {
                out.push_str(&format!(
                    "[{i}] kind={:?} count={}\n",
                    entry.kind, entry.count
                ));
            }
        }
        TableKind::Sections => {
            out.push_str(&format!("Sections ({} total):\n", file.sections.len()));
            out.push_str("----------------------------------------\n");
            out.push_str(&format!(
                "{:<22} {:<12} {:<12} {:<8}\n",
                "Name", "Offset", "Size", "Entries"
            ));
            for s in &file.sections {
                let entries = s
                    .entries
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "-".to_string());
                out.push_str(&format!(
                    "{:<22} {:<12} {:<12} {:<8}\n",
                    s.name,
                    format!("0x{:x}", s.offset),
                    s.size,
                    entries
                ));
            }
        }
        TableKind::BigInt => {
            out.push_str(&format!(
                "BigInt Table ({} entries, storage {} bytes):\n",
                file.big_int_table.len(),
                file.big_int_storage.len()
            ));
            out.push_str("----------------------------------------\n");
            for (i, entry) in file.big_int_table.iter().enumerate() {
                let bytes = slice_from(&file.big_int_storage, entry.offset, entry.length);
                out.push_str(&format!(
                    "[{i}] offset={} length={} bytes=[{}]\n",
                    entry.offset,
                    entry.length,
                    hex_preview(bytes, 16)
                ));
            }
        }
        TableKind::ArrayBuffer => {
            out.push_str("Array Buffer summary:\n");
            out.push_str("----------------------------------------\n");
            out.push_str(&format!("size: {} bytes\n", file.array_buffer.len()));
            out.push_str(&format!(
                "literal_value_buffer: {} bytes\n",
                file.literal_value_buffer.len()
            ));
            out.push_str(&format!("obj_key_buffer: {} bytes\n", file.obj_key_buffer.len()));
            out.push_str(&format!(
                "obj_value_buffer: {} bytes\n",
                file.obj_value_buffer.len()
            ));
            out.push_str(&format!("preview: [{}]\n", hex_preview(&file.array_buffer, 32)));
        }
    }
    out
}

// Dump a structural table as JSON.
pub fn dump_table_json(file: &BytecodeFile, kind: TableKind) -> Value {
    match kind {
        TableKind::CjsModules => Value::Array(
            file.cjs_module_table
                .iter()
                .enumerate()
                .map(|(i, (symbol_id, function_id))| {
                    json!({ "index": i, "symbol_id": symbol_id, "function_id": function_id })
                })
                .collect(),
        ),
        TableKind::RegExp => Value::Array(
            file.reg_exp_table
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let bytes = slice_from(&file.reg_exp_storage, e.offset, e.length);
                    json!({
                        "index": i,
                        "offset": e.offset,
                        "length": e.length,
                        "bytecode_hex": bytes.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                    })
                })
                .collect(),
        ),
        TableKind::ObjShapes => Value::Array(
            file.obj_shape_table
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    json!({ "index": i, "key_buffer_offset": e.key_buffer_offset, "num_props": e.num_props })
                })
                .collect(),
        ),
        TableKind::FunctionSources => Value::Array(
            file.function_source_table
                .iter()
                .enumerate()
                .map(|(i, (function_id, string_id))| {
                    json!({
                        "index": i,
                        "function_id": function_id,
                        "string_id": string_id,
                        "source": file.string_at(*string_id).map(|e| e.value.clone()),
                    })
                })
                .collect(),
        ),
        TableKind::StringKinds => Value::Array(
            file.string_kinds
                .iter()
                .enumerate()
                .map(|(i, e)| json!({ "index": i, "kind": format!("{:?}", e.kind), "count": e.count }))
                .collect(),
        ),
        TableKind::Sections => Value::Array(
            file.sections
                .iter()
                .map(|s| {
                    json!({ "name": s.name, "offset": s.offset, "size": s.size, "entries": s.entries })
                })
                .collect(),
        ),
        TableKind::BigInt => Value::Array(
            file.big_int_table
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let bytes = slice_from(&file.big_int_storage, e.offset, e.length);
                    json!({
                        "index": i,
                        "offset": e.offset,
                        "length": e.length,
                        "bytes_hex": bytes.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                    })
                })
                .collect(),
        ),
        TableKind::ArrayBuffer => json!({
            "array_buffer_size": file.array_buffer.len(),
            "literal_value_buffer_size": file.literal_value_buffer.len(),
            "obj_key_buffer_size": file.obj_key_buffer.len(),
            "obj_value_buffer_size": file.obj_value_buffer.len(),
            "preview_hex": file.array_buffer.iter().take(32).map(|b| format!("{b:02x}")).collect::<String>(),
        }),
    }
}

// Per-header register-count description (differs between Legacy and Modern).
fn register_summary(header: &FunctionHeader) -> String {
    match header {
        FunctionHeader::Legacy(h) => format!("env={}", h.environment_size),
        FunctionHeader::Modern(h) => format!(
            "regs={}(num={},nonptr={})",
            h.number_reg_count + h.non_ptr_reg_count,
            h.number_reg_count,
            h.non_ptr_reg_count
        ),
    }
}

// Build a one-line metadata banner for a single function.
//
// Returns `None` if `function_id` is out of range.
pub fn function_info_banner(file: &BytecodeFile, function_id: u32) -> Option<String> {
    let header = file.function_headers.get(function_id as usize)?;
    let name = file
        .string_at(header.function_name())
        .map(|e| e.value.clone())
        .unwrap_or_default();
    let name = if name.is_empty() {
        "<anonymous>".to_string()
    } else {
        name
    };

    let mut flags = Vec::new();
    if header.is_strict() {
        flags.push("strict");
    }
    if header.is_overflowed() {
        flags.push("overflowed");
    }
    if header.prohibit_construct() {
        flags.push("no-construct");
    }
    let flags_str = if flags.is_empty() {
        String::new()
    } else {
        format!(" flags=[{}]", flags.join(","))
    };

    let eh = file
        .exception_handlers
        .get(&function_id)
        .map(|v| v.len())
        .unwrap_or(0);
    let eh_str = if eh > 0 {
        format!(" exc_handlers={eh}")
    } else {
        String::new()
    };

    Some(format!(
        "fn#{id} \"{name}\" params={params} frame={frame} {regs} size={size} offset=0x{offset:x}{flags}{eh}",
        id = function_id,
        name = name,
        params = header.param_count(),
        frame = header.frame_size(),
        regs = register_summary(header),
        size = header.bytecode_size_in_bytes(),
        offset = header.offset(),
        flags = flags_str,
        eh = eh_str,
    ))
}

fn fn_name(file: &BytecodeFile, id: u32) -> String {
    file.function_headers
        .get(id as usize)
        .and_then(|h| file.string_at(h.function_name()))
        .map(|e| e.value.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("f{id}"))
}

// Restrict an edge map to nodes reachable from `root` within `depth` hops.
fn reachable_within(
    calls: &BTreeMap<u32, Vec<u32>>,
    root: u32,
    depth: usize,
) -> BTreeSet<u32> {
    let mut keep = BTreeSet::new();
    keep.insert(root);
    let mut queue: VecDeque<(u32, usize)> = VecDeque::new();
    queue.push_back((root, 0));
    while let Some((node, d)) = queue.pop_front() {
        if d >= depth {
            continue;
        }
        if let Some(callees) = calls.get(&node) {
            for &c in callees {
                if keep.insert(c) {
                    queue.push_back((c, d + 1));
                }
            }
        }
    }
    keep
}

// Build the bundle call graph and render it as text or Graphviz DOT.
//
// * `root` — if set, restrict to the subgraph reachable from that function.
// * `depth` — max hops from `root` (ignored when `root` is `None`).
// * `dot` — emit Graphviz DOT instead of a text edge listing.
pub fn render_call_graph(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    root: Option<u32>,
    depth: usize,
    dot: bool,
) -> Result<String> {
    let analysis = crate::analyze_module(file, format)?;
    let calls = &analysis.graph.calls;

    // Determine which nodes (callers) we render edges for.
    let allowed: Option<BTreeSet<u32>> =
        root.map(|r| reachable_within(calls, r, depth));

    let node_allowed =
        |id: u32| allowed.as_ref().map(|s| s.contains(&id)).unwrap_or(true);

    let mut out = String::new();

    if dot {
        out.push_str("digraph callgraph {\n");
        out.push_str("  node [shape=box, fontname=\"monospace\"];\n");
        let mut seen_nodes = BTreeSet::new();
        for (&caller, callees) in calls {
            if !node_allowed(caller) {
                continue;
            }
            for &callee in callees {
                if !node_allowed(callee) {
                    continue;
                }
                for n in [caller, callee] {
                    if seen_nodes.insert(n) {
                        out.push_str(&format!(
                            "  n{n} [label=\"{n}: {}\"];\n",
                            fn_name(file, n).replace('"', "\\\"")
                        ));
                    }
                }
                out.push_str(&format!("  n{caller} -> n{callee};\n"));
            }
        }
        out.push_str("}\n");
    } else {
        let title = match root {
            Some(r) => format!(
                "Call graph from function {r} ({}) depth {depth}:\n",
                fn_name(file, r)
            ),
            None => format!("Call graph ({} callers with edges):\n", calls.len()),
        };
        out.push_str(&title);
        out.push_str("----------------------------------------\n");
        for (&caller, callees) in calls {
            if !node_allowed(caller) {
                continue;
            }
            let shown: Vec<u32> = callees
                .iter()
                .copied()
                .filter(|&c| node_allowed(c))
                .collect();
            if shown.is_empty() {
                continue;
            }
            // De-duplicate callees while keeping deterministic order.
            let unique: BTreeSet<u32> = shown.into_iter().collect();
            let list = unique
                .iter()
                .map(|&c| format!("{c} ({})", fn_name(file, c)))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("{caller} ({}) -> {list}\n", fn_name(file, caller)));
        }
    }

    Ok(out)
}
