use hbc_decomp::{
    BytecodeFile, BytecodeFormat, ClosureInfo, DecompileOptionsV2, Decompiler, IRBuilder,
    IRBuilderOptions, StructureAnalysis,
};
use regex::Regex;
use std::collections::HashSet;
use std::error::Error;
use std::fmt::Write as _;

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

    println!("=== Closure mappings for function {function_id} ===\n");

    if closure_info.slots.is_empty() {
        println!("No closure slots found.");
    } else {
        let mut slots: Vec<_> = closure_info.slots.iter().collect();
        slots.sort_by_key(|(k, _)| *k);

        for (slot, value) in slots {
            let desc = match value {
                hbc_decomp::ClosureSlotValue::Function { id, name } => {
                    if let Some(n) = name {
                        format!("F{id} ({n})")
                    } else {
                        format!("F{id}")
                    }
                }
                hbc_decomp::ClosureSlotValue::Constant(c) => format!("constant: {c}"),
                hbc_decomp::ClosureSlotValue::Variable(v) => format!("variable: {v}"),
                hbc_decomp::ClosureSlotValue::Unknown => "unknown".to_string(),
            };
            println!("  closure_{slot} = {desc}");
        }
    }

    Ok(())
}

pub fn expand_json(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    options: &DecompileOptionsV2,
) -> Result<String, Box<dyn Error>> {
    let decompiler = Decompiler::from_parts(file.clone(), format.clone());
    let ir = decompiler.decompile_to_ir(function_id, options)?;
    let json = serde_json::to_string_pretty(&ir)?;
    Ok(json)
}

// Format the section header table for assembly mode output.
fn format_section_header(
    file: &BytecodeFile,
    file_path: &str,
    file_size: usize,
) -> String {
    let mut out = String::new();

    let layout_str = match file.header.layout {
        hbc_decomp::HeaderLayout::Legacy => "Legacy",
        hbc_decomp::HeaderLayout::Modern => "Modern",
    };

    let hash_hex: String = file.header.source_hash.iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    let _ = writeln!(out, "=== Hermes Bytecode v{} ({}) ===", file.header.version, layout_str);
    let _ = writeln!(out, "File: {} ({} bytes)", file_path, format_count(file_size as u32));
    let _ = writeln!(out, "Source hash: {hash_hex}");
    let _ = writeln!(out);

    // Section table header
    let _ = writeln!(out,
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
        let _ = writeln!(out,
            "{:<24} 0x{:08x}  {:>10}  {:>4.1}%  {:>8}",
            sec.name, sec.offset, format_size(sec.size as usize), pct, entries_str
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
            // This is an offset marker line — store the offset and skip the line
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
