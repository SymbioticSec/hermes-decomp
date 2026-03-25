use hbc_decomp::{BytecodeFile, DebugInfo};
use std::error::Error;

pub fn print_info(file: &BytecodeFile) {
    println!("Hermes Bytecode Info");
    println!("  Version: {}", file.header.version);
    println!("  Layout: {:?}", file.header.layout);
    println!(
        "  Function header layout: {:?}",
        file.header.function_header_layout
    );
    println!("  Functions: {}", file.header.function_count);
    println!("  Strings: {}", file.header.string_count);
    println!("  Identifiers: {}", file.header.identifier_count);
    println!("  RegExp: {}", file.header.reg_exp_count);
    println!("  CJS Modules: {}", file.header.cjs_module_count);
    if let Some(count) = file.header.big_int_count {
        println!("  BigInt: {count}");
    }
    if let Some(count) = file.header.function_source_count {
        println!("  Function sources: {count}");
    }
    println!("  Instruction offset: {}", file.instruction_offset);
}

pub fn print_debug_info(
    file: &BytecodeFile,
    bytes: &[u8],
    scopes: bool,
    callees: bool,
    vars: bool,
) -> Result<(), Box<dyn Error>> {
    let debug_offset = file.header.debug_info_offset;
    println!("=== Debug Info ===");
    println!("Debug info offset: {debug_offset} (0x{debug_offset:x})");
    println!();

    if debug_offset == 0 || debug_offset == u32::MAX {
        println!("No debug info section found in this bytecode file.");
        println!("(This is common for release builds)");
        return Ok(());
    }

    let debug_info = match DebugInfo::parse(bytes, debug_offset) {
        Ok(info) => info,
        Err(e) => {
            println!("Failed to parse debug info: {e}");
            return Ok(());
        }
    };

    let show_all = !scopes && !callees && !vars;

    // Show scope descriptors
    if show_all || scopes {
        println!(
            "=== Scope Descriptors ({}) ===",
            debug_info.scope_descriptors.len()
        );
        for scope in &debug_info.scope_descriptors {
            println!("  Scope at offset {}:", scope.offset);
            if let Some(parent) = scope.parent_offset {
                println!("    Parent: {parent}");
            } else {
                println!("    Parent: (none - root scope)");
            }
            println!(
                "    Flags: {} (inner={}, dynamic={})",
                scope.flags,
                scope.is_inner_scope(),
                scope.is_dynamic()
            );
            if !scope.names.is_empty() {
                println!("    Variables ({}):", scope.names.len());
                for (i, name) in scope.names.iter().enumerate() {
                    if !name.is_empty() {
                        println!("      [{i}] {name}");
                    }
                }
            }
        }
        println!();
    }

    // Show textified callees
    if show_all || callees {
        println!(
            "=== Textified Callees ({}) ===",
            debug_info.textified_callees.len()
        );
        let mut callees_vec: Vec<_> = debug_info.textified_callees.iter().collect();
        callees_vec.sort_by_key(|(addr, _)| *addr);
        for (addr, name) in callees_vec {
            println!("  0x{addr:04x}: {name}");
        }
        println!();
    }

    // Show all unique variable names
    if show_all || vars {
        let all_names = debug_info.all_variable_names();
        println!("=== All Variable Names ({}) ===", all_names.len());
        let mut unique: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for name in &all_names {
            unique.insert(name);
        }
        let mut sorted: Vec<_> = unique.into_iter().collect();
        sorted.sort();
        for name in sorted {
            println!("  {name}");
        }
        println!();
    }

    // Show string table
    if show_all && !debug_info.string_table.is_empty() {
        println!(
            "=== Debug String Table ({}) ===",
            debug_info.string_table.len()
        );
        for (i, s) in debug_info.string_table.iter().enumerate().take(50) {
            println!("  [{i}] {s}");
        }
        if debug_info.string_table.len() > 50 {
            println!("  ... ({} more)", debug_info.string_table.len() - 50);
        }
    }

    Ok(())
}
