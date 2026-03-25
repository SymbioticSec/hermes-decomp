use std::collections::{BTreeMap, HashSet};
use crate::analysis::ClosureContext;
use crate::error::Result;
use crate::file::BytecodeFile;
use crate::opcode::BytecodeFormat;

use super::{
    build_function_name_index, generate_ir,
    DecompileOptionsV2, PipelineContext,
};

pub fn decompile_all_v2_with_closures(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    options: &DecompileOptionsV2,
) -> Result<String> {
    let pipeline = PipelineContext::build_with_options(file, format, options)?;

    let mut output = String::new();
    let mut module_functions: BTreeMap<u32, Vec<u32>> =
        BTreeMap::new();
    let mut orphans: Vec<u32> = Vec::new();

    let get_root = |func_id: u32| -> u32 {
        if pipeline.registry.function_to_module.contains_key(&func_id) {
            return func_id;
        }

        if let Some(ctx) = &pipeline.closure_ctx {
            let mut visited = HashSet::new();
            let mut current = func_id;

            while let Some(&parent) = ctx.parent_function.get(&current) {
                if !visited.insert(current) {
                    break;
                }
                if pipeline.registry.function_to_module.contains_key(&parent) {
                    return parent;
                }
                current = parent;
            }
            return current;
        }
        func_id
    };

    let child_functions: HashSet<u32> = if let Some(ctx) = &pipeline.closure_ctx {
        ctx.parent_function
            .keys()
            .copied()
            .filter(|id| !pipeline.registry.function_to_module.contains_key(id))
            .collect()
    } else {
        HashSet::new()
    };

    for i in 0..file.header.function_count {
        if pipeline.all_ir.contains_key(&i) && !child_functions.contains(&i) {
            let root_id = get_root(i);
            if let Some(module) = pipeline.registry.get_module_for_function(root_id) {
                module_functions
                    .entry(module.module_id)
                    .or_default()
                    .push(i);
            } else {
                orphans.push(i);
            }
        }
    }

    let mut sorted_modules: Vec<_> = module_functions.keys().cloned().collect();
    sorted_modules.sort();

    // Print Modules
    for mod_id in sorted_modules {
        let name = pipeline.registry
            .get_module(mod_id)
            .and_then(|m| m.name.as_deref())
            .unwrap_or("?");
        output.push_str(&format!("// === Module {mod_id}: {name} ===\n"));

        if let Some(funcs) = module_functions.get_mut(&mod_id) {
            funcs.sort();
            for &func_id in funcs.iter() {
                if !output.is_empty() { output.push('\n'); }
                output.push_str(&pipeline.generate_function_code(file, func_id));
            }
        }
    }

    if !orphans.is_empty() {
        output.push_str("\n// === Orphan Functions ===\n");
        orphans.sort();
        for func_id in orphans {
            if !output.is_empty() { output.push('\n'); }
            output.push_str(&pipeline.generate_function_code(file, func_id));
        }
    }

    Ok(output)
}

pub fn analyze_module(
    file: &BytecodeFile,
    format: &BytecodeFormat,
) -> Result<crate::analysis::GlobalAnalysis> {
    let mut registry = crate::analysis::MetroRegistry::new();
    let raw_options = DecompileOptionsV2 {
        resolve_strings: true,
        ..DecompileOptionsV2::default()
    };

    for i in 0..file.header.function_count {
        if let Ok(stmts) = generate_ir(file, format, i, &raw_options, None, false) {
            registry.analyze_statements(&stmts);
        }
    }

    let options = DecompileOptionsV2::default();
    let mut all_ir = BTreeMap::new();

    for i in 0..file.header.function_count {
        if let Ok(statements) = generate_ir(file, format, i, &options, None, false) {
            all_ir.insert(i, statements);
        }
    }

    let mut closure_ctx: Option<ClosureContext> = None;
    crate::analysis::metro::propagate_module_names(&mut all_ir, &mut registry, &mut closure_ctx);

    for module in registry.modules.values_mut() {
        crate::analysis::metro::exports::ExportAnalyzer::analyze(module, &all_ir);
    }

    let func_name_index = build_function_name_index(file);

    let global_analysis = crate::analysis::run_ipa(&all_ir, &registry, &func_name_index);

    Ok(global_analysis)
}
