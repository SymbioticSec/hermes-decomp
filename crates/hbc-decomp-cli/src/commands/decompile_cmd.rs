use crate::helpers::function_name;
use hbc_decomp::{
    BytecodeFile, BytecodeFormat, ClosureInfo, ClosureSlotValue, DecompileOptionsV2, IRBuilder,
    IRBuilderOptions, ModuleFilter, PipelineContext, Statement, StructureAnalysis,
};
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::error::Error;
use std::fmt::Write as _;
use std::io::Write;

// Decompile a function and expand all referenced functions up to a certain depth.
pub fn decompile_with_expansion(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    root_function_id: u32,
    options: &DecompileOptionsV2,
    max_depth: usize,
) -> Result<String, Box<dyn Error>> {
    let mut output = String::new();
    let mut decompiled: HashSet<u32> = HashSet::new();
    let mut queue: Vec<(u32, usize)> = vec![(root_function_id, 0)];

    // Regex to find function references like /* F123 */
    let func_ref_re = Regex::new(r"/\* F(\d+) \*/").unwrap();

    while let Some((func_id, depth)) = queue.pop() {
        if decompiled.contains(&func_id) {
            continue;
        }
        decompiled.insert(func_id);

        // Add separator for nested functions
        if !output.is_empty() {
            output.push_str("\n// ========================================\n");
            output.push_str(&format!("// Referenced function F{func_id}\n"));
            output.push_str("// ========================================\n\n");
        }

        // Decompile the function
        let func_output = hbc_decomp::decompile_function_v2(file, format, func_id, options)?;
        output.push_str(&func_output);

        // If we haven't reached max depth, find and queue referenced functions
        if depth < max_depth {
            for cap in func_ref_re.captures_iter(&func_output) {
                if let Ok(ref_id) = cap[1].parse::<u32>() {
                    if !decompiled.contains(&ref_id) {
                        queue.push((ref_id, depth + 1));
                    }
                }
            }
        }
    }

    // Add summary
    output.push_str(&format!(
        "\n// ========================================\n\
         // Expansion summary: {} functions decompiled\n\
         // Root: F{}, Max depth: {}\n\
         // ========================================\n",
        decompiled.len(),
        root_function_id,
        max_depth
    ));

    Ok(output)
}

pub fn print_closure_info(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    json: bool,
) -> Result<(), Box<dyn Error>> {
    let options = IRBuilderOptions {
        resolve_strings: true,
        include_offsets: false,
        ..Default::default()
    };
    let mut builder = IRBuilder::new(file, format, options);
    let cfg = builder.build_function(function_id)?;

    // Get structured statements
    let analysis = StructureAnalysis::analyze(&cfg);
    let statements = analysis.root.to_statements(&cfg);

    // Analyze closures
    let closure_info = ClosureInfo::analyze(&statements);

    if json {
        let doc = closures_json(function_id, &closure_info);
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }

    println!("=== Closure mappings for function {function_id} ===\n");

    if closure_info.slots.is_empty() {
        println!("No closure slots found.");
    } else {
        let mut slots: Vec<_> = closure_info.slots.iter().collect();
        slots.sort_by_key(|(k, _)| *k);

        for (slot, value) in slots {
            let desc = match value {
                ClosureSlotValue::Function { id, name } => {
                    if let Some(n) = name {
                        format!("F{id} ({n})")
                    } else {
                        format!("F{id}")
                    }
                }
                ClosureSlotValue::Constant(c) => format!("constant: {c}"),
                ClosureSlotValue::RegExp => "regexp".to_string(),
                ClosureSlotValue::Variable(v) => format!("variable: {v}"),
                ClosureSlotValue::Unknown => "unknown".to_string(),
            };
            println!("  closure_{slot} = {desc}");
        }
    }

    Ok(())
}

// One row per closure slot, in slot order, as a JSON document.
pub fn closures_json(function_id: u32, info: &ClosureInfo) -> Value {
    let slots: Vec<Value> = info
        .slots
        .iter()
        .map(|(slot, value)| match value {
            ClosureSlotValue::Function { id, name } => {
                json!({ "slot": slot, "kind": "function", "function_id": id, "name": name })
            }
            ClosureSlotValue::Constant(c) => {
                json!({ "slot": slot, "kind": "constant", "value": c })
            }
            ClosureSlotValue::RegExp => json!({ "slot": slot, "kind": "regexp" }),
            ClosureSlotValue::Variable(v) => json!({ "slot": slot, "kind": "variable", "name": v }),
            ClosureSlotValue::Unknown => json!({ "slot": slot, "kind": "unknown" }),
        })
        .collect();
    json!({ "function_id": function_id, "slots": slots })
}

// Report the functions the call graph cannot reach from the Metro roots.
pub fn print_dead_code_report(
    file: &BytecodeFile,
    format: &BytecodeFormat,
) -> Result<(), Box<dyn Error>> {
    let analysis = hbc_decomp::analyze_module(file, format)?;
    println!("Dead Code Analysis:");
    println!("-------------------");
    if analysis.dead_code.is_empty() {
        println!("No unreachable functions detected.");
    } else {
        let mut dead: Vec<u32> = analysis.dead_code.into_iter().collect();
        dead.sort();
        println!("Found {} unreachable functions:", dead.len());
        for id in dead {
            let name = file
                .string_at(file.function_headers[id as usize].function_name())
                .map(|e| e.value.as_str())
                .unwrap_or("");
            println!("  Function {id} ({name})");
        }
    }
    Ok(())
}

// The Metro module a function belongs to: its own when it is a factory, else
// the first factory up its closure parent chain.
fn module_of(ctx: &PipelineContext, function_id: u32) -> Option<u32> {
    let registry = &ctx.registry;
    if let Some(m) = registry.get_module_for_function(function_id) {
        return Some(m.module_id);
    }
    let parents = &ctx.closure_ctx.as_ref()?.parent_function;
    let mut seen = HashSet::new();
    let mut current = function_id;
    while let Some(&parent) = parents.get(&current) {
        if !seen.insert(current) {
            break;
        }
        if let Some(m) = registry.get_module_for_function(parent) {
            return Some(m.module_id);
        }
        current = parent;
    }
    None
}

// A function `decompile --json` emits: its id and the Metro module it belongs to.
pub type SelectedFunction = (u32, Option<u32>);

// The functions `decompile --json` emits, as (function id, module id) pairs in
// id order. `--function` selects one; otherwise a module filter keeps the
// functions of the selected modules (orphans are dropped, as in the text path),
// and no filter keeps everything the pipeline produced IR for.
pub fn select_json_functions(
    ctx: &PipelineContext,
    function: Option<u32>,
    filter: &ModuleFilter,
) -> Result<Vec<SelectedFunction>, Box<dyn Error>> {
    if let Some(id) = function {
        if !ctx.all_ir.contains_key(&id) {
            return Err(format!("function {id} has no IR").into());
        }
        return Ok(vec![(id, module_of(ctx, id))]);
    }
    let allowed = (!filter.is_empty()).then(|| filter.resolve(&ctx.registry));
    Ok(ctx
        .all_ir
        .keys()
        .filter_map(|&id| {
            let module = module_of(ctx, id);
            match &allowed {
                None => Some((id, module)),
                Some(set) => module.filter(|m| set.contains(m)).map(|m| (id, Some(m))),
            }
        })
        .collect())
}

#[derive(Serialize)]
struct FunctionJson<'a> {
    id: u32,
    name: Option<String>,
    module: Option<u32>,
    body: &'a [Statement],
}

// Stream `{"version":N,"functions":[...]}` to `w`, one function per line, so a
// whole-bundle dump never has to be held as a single string.
pub fn write_ir_json<W: Write>(
    w: &mut W,
    file: &BytecodeFile,
    ctx: &PipelineContext,
    selected: &[SelectedFunction],
) -> Result<(), Box<dyn Error>> {
    write!(w, "{{\"version\":{},\"functions\":[", file.header.version)?;
    let mut first = true;
    for &(id, module) in selected {
        let Some(body) = ctx.all_ir.get(&id) else {
            continue;
        };
        w.write_all(if first { b"\n" } else { b",\n" })?;
        first = false;
        let row = FunctionJson {
            id,
            name: function_name(file, id),
            module,
            body,
        };
        serde_json::to_writer(&mut *w, &row)?;
    }
    w.write_all(b"\n]}\n")?;
    Ok(())
}

// Format the section header table for assembly mode output.
fn format_section_header(file: &BytecodeFile, file_path: &str, file_size: usize) -> String {
    let mut out = String::new();

    let layout_str = match file.header.layout {
        hbc_decomp::HeaderLayout::Legacy => "Legacy",
        hbc_decomp::HeaderLayout::Modern => "Modern",
    };

    let hash_hex: String = file
        .header
        .source_hash
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    let _ = writeln!(
        out,
        "=== Hermes Bytecode v{} ({}) ===",
        file.header.version, layout_str
    );
    let _ = writeln!(
        out,
        "File: {} ({} bytes)",
        file_path,
        format_count(file_size as u32)
    );
    let _ = writeln!(out, "Source hash: {hash_hex}");
    let _ = writeln!(out);

    // Section table header
    let _ = writeln!(
        out,
        "{:<24} {:>10}  {:>12}  {:>5}  {:>8}",
        "Section", "Offset", "Size", "%", "Entries"
    );
    let _ = writeln!(out, "{}", "\u{2500}".repeat(68));

    for sec in &file.sections {
        let pct = if file_size > 0 {
            (sec.size as f64 / file_size as f64) * 100.0
        } else {
            0.0
        };
        let entries_str = match sec.entries {
            Some(n) => format_count(n).to_string(),
            None => String::new(),
        };
        let _ = writeln!(
            out,
            "{:<24} 0x{:08x}  {:>10}  {:>4.1}%  {:>8}",
            sec.name,
            sec.offset,
            format_size(sec.size as usize),
            pct,
            entries_str
        );
    }

    let _ = writeln!(out, "{}", "\u{2500}".repeat(68));
    let _ = writeln!(out);
    out
}

// Format a byte size with comma separators and unit suffix.
fn format_size(bytes: usize) -> String {
    if bytes == 0 {
        return "0 B".to_string();
    }
    format!("{} B", format_count(bytes as u32))
}

// Format a number with comma separators.
fn format_count(n: u32) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

// Post-process decompiled output for assembly mode.
//
// Converts inline `// @XXXXXXXX` offset comments into left-margin offsets
// in Binary Ninja style: `0xXXXXXXXX | code here`.
pub fn format_assembly_output(
    raw_output: &str,
    file: &BytecodeFile,
    file_path: &str,
    file_size: usize,
) -> String {
    let header = format_section_header(file, file_path, file_size);
    let mut out = header;

    // Regex to match offset comment lines: "// @XXXXXXXX" (8 hex digits for absolute offsets)
    let offset_re = Regex::new(r"^\s*// @([0-9a-fA-F]{8})\s*$").unwrap();

    let mut current_offset: Option<String> = None;
    let margin_empty = "           | ";
    // margin_empty is "           | " (11 chars for "0x" + 8 hex + " | ")

    for line in raw_output.lines() {
        if let Some(caps) = offset_re.captures(line) {
            // This is an offset marker line, store the offset and skip the line
            current_offset = Some(caps[1].to_string());
            continue;
        }

        // Emit the line with the current offset as left margin
        if let Some(ref offset) = current_offset {
            let _ = writeln!(out, "0x{offset} | {line}");
            current_offset = None;
        } else {
            let _ = writeln!(out, "{margin_empty}{line}");
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn minimal_context() -> (BytecodeFile, PipelineContext) {
        let bytes = hbc_decomp::create_minimal(&hbc_decomp::CreateOptions {
            version: 96,
            strings: vec!["global".into()],
            ..Default::default()
        })
        .unwrap();
        let file = BytecodeFile::parse_auto(&bytes).unwrap();
        let format = BytecodeFormat::for_version(96).unwrap();
        let ctx =
            PipelineContext::build_with_options(&file, &format, &DecompileOptionsV2::optimized())
                .unwrap();
        (file, ctx)
    }

    #[test]
    fn ir_json_is_one_document_per_bundle() {
        let (file, ctx) = minimal_context();
        let selected = select_json_functions(&ctx, None, &ModuleFilter::default()).unwrap();
        assert_eq!(selected.len(), 1);
        let mut out = Vec::new();
        write_ir_json(&mut out, &file, &ctx, &selected).unwrap();
        let doc: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(doc["version"], 96);
        let functions = doc["functions"].as_array().unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0]["id"], 0);
        assert!(functions[0]["module"].is_null());
        assert!(functions[0]["body"].is_array());
    }

    #[test]
    fn ir_json_single_function_must_exist() {
        let (file, ctx) = minimal_context();
        assert!(select_json_functions(&ctx, Some(99), &ModuleFilter::default()).is_err());
        let selected = select_json_functions(&ctx, Some(0), &ModuleFilter::default()).unwrap();
        let mut out = Vec::new();
        write_ir_json(&mut out, &file, &ctx, &selected).unwrap();
        let doc: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(doc["functions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn ir_json_module_filter_drops_orphans() {
        let (_, ctx) = minimal_context();
        let filter = ModuleFilter {
            id_ranges: vec![(0, 0)],
            ..Default::default()
        };
        // The minimal bundle has no Metro module, so a module filter selects nothing.
        assert!(select_json_functions(&ctx, None, &filter)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn closures_json_describes_each_slot() {
        let mut info = ClosureInfo::new();
        info.slots = BTreeMap::from([
            (
                0,
                ClosureSlotValue::Function {
                    id: 7,
                    name: Some("cb".into()),
                },
            ),
            (1, ClosureSlotValue::Constant("42".into())),
            (2, ClosureSlotValue::RegExp),
            (3, ClosureSlotValue::Variable("state".into())),
            (4, ClosureSlotValue::Unknown),
        ]);
        let doc = closures_json(3, &info);
        let back: Value = serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert_eq!(back["function_id"], 3);
        let slots = back["slots"].as_array().unwrap();
        assert_eq!(slots.len(), 5);
        assert_eq!(slots[0]["kind"], "function");
        assert_eq!(slots[0]["function_id"], 7);
        assert_eq!(slots[0]["name"], "cb");
        assert_eq!(slots[1]["value"], "42");
        assert_eq!(slots[2]["kind"], "regexp");
        assert_eq!(slots[3]["name"], "state");
        assert_eq!(slots[4]["kind"], "unknown");
    }
}
