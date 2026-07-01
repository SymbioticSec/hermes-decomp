use hbc_decomp::{BytecodeFile, BytecodeFormat, DecompileOptionsV2, PipelineContext};
use std::error::Error;
use std::fs;
use std::path::Path;

// Build (or load from the on-disk cache) the full analysis pipeline for a file.
fn build_cached_pipeline(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    bytes: &[u8],
    cache_path: &Path,
) -> Result<PipelineContext, Box<dyn Error>> {
    Ok(PipelineContext::build_cached(
        file,
        format,
        &DecompileOptionsV2::optimized(),
        bytes,
        cache_path,
    )?)
}

pub fn run_extract(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    output_dir: &Path,
    bytes: &[u8],
    cache_path: &Path,
    // The full pipeline always resolves strings; kept for CLI signature stability.
    _resolve_strings: bool,
) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(output_dir)?;
    println!("Extracting modules to {}...", output_dir.display());

    // Build the full analysis pipeline once (cached): this resolves module names,
    // exports and produces the same full-quality (ESM) output as `decompile`.
    let ctx = build_cached_pipeline(file, format, bytes, cache_path)?;

    println!("Found {} modules.", ctx.registry.modules.len());

    let mut modules: Vec<_> = ctx.registry.modules.values().cloned().collect();
    modules.sort_by_key(|m| m.module_id);

    for module in &modules {
        // Prefix with the module ID so distinct modules that share an inferred
        // name (common in large bundles) never overwrite each other.
        let filename = if let Some(name) = &module.name {
            let safe_name = name.replace(['/', '\\'], "_");
            format!("{}_{safe_name}.js", module.module_id)
        } else {
            format!("module_{}.js", module.module_id)
        };
        let path = output_dir.join(filename);

        print!(
            "Extracting module {} (F{})... ",
            module.module_id, module.function_id
        );

        let code = ctx.generate_function_code(file, module.function_id);

        // Add header
        let mut content = String::new();
        content.push_str(&format!("// Module ID: {}\n", module.module_id));
        content.push_str(&format!("// Function ID: {}\n", module.function_id));
        if let Some(name) = &module.name {
            content.push_str(&format!("// Name: {name}\n"));
        }
        content.push_str(&format!("// Dependencies: {:?}\n", module.dependencies));
        if !module.exports.is_empty() {
            let mut names: Vec<_> = module.exports.keys().cloned().collect();
            names.sort();
            content.push_str(&format!("// Exports: {}\n", names.join(", ")));
        }
        content.push('\n');
        content.push_str(&code);

        fs::write(&path, content)?;
        println!("OK");
    }

    Ok(())
}

pub fn print_modules(
    file: &BytecodeFile,
    format: &hbc_decomp::BytecodeFormat,
    bytes: &[u8],
    cache_path: &Path,
    limit: Option<usize>,
) -> Result<(), Box<dyn Error>> {
    // Use the full analysis pipeline so module names and exports are resolved
    // (the lightweight detection registry only knows IDs and dependencies).
    let ctx = build_cached_pipeline(file, format, bytes, cache_path)?;
    let registry = &ctx.registry;

    println!("=== Metro Modules ===\n");
    println!("Total modules: {}\n", registry.modules.len());

    let mut modules: Vec<_> = registry.modules.values().collect();
    modules.sort_by_key(|m| m.module_id);

    let display_count = limit.unwrap_or(modules.len()).min(modules.len());

    for module in modules.iter().take(display_count) {
        let name_str = module
            .name
            .as_ref()
            .map(|n| format!(" - {n}"))
            .unwrap_or_default();
        let deps_str = if module.dependencies.is_empty() {
            String::new()
        } else {
            format!(
                " deps: [{}]",
                module
                    .dependencies
                    .iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let exports_str = if module.exports.is_empty() {
            String::new()
        } else {
            format!(" exports: {}", module.exports.len())
        };
        println!(
            "Module {} (F{}){}{}{}",
            module.module_id, module.function_id, name_str, deps_str, exports_str
        );
    }

    if display_count < modules.len() {
        println!("\n... and {} more modules", modules.len() - display_count);
    }

    Ok(())
}

pub fn print_module_deps(
    file: &BytecodeFile,
    format: &hbc_decomp::BytecodeFormat,
    bytes: &[u8],
    cache_path: &Path,
    module_id: u32,
    depth: usize,
) -> Result<(), Box<dyn Error>> {
    // Full pipeline so dependency names (not just IDs) are available.
    let ctx = build_cached_pipeline(file, format, bytes, cache_path)?;
    let registry = &ctx.registry;

    println!("=== Module {module_id} dependencies ===\n");

    if let Some(module) = registry.get_module(module_id) {
        println!("Module ID: {}", module.module_id);
        println!("Function ID: F{}", module.function_id);
        if let Some(name) = &module.name {
            println!("Name: {name}");
        }
        println!("\nDirect dependencies ({}):", module.dependencies.len());
        for &dep_id in &module.dependencies {
            let dep_info = registry
                .get_module(dep_id)
                .map(|m| format!(" -> F{}", m.function_id))
                .unwrap_or_default();
            println!("  Module {dep_id}{dep_info}");
        }

        println!("\nDependency tree (depth {depth}):");
        let tree = registry.get_dependency_tree(module_id, depth);
        print!("{}", tree.format(1));

        println!("\nDependent modules (modules that require this one):");
        let dependents = registry.get_dependents(module_id);
        if dependents.is_empty() {
            println!("  None found");
        } else {
            for dep_id in dependents.iter().take(20) {
                let dep_info = registry
                    .get_module(*dep_id)
                    .map(|m| format!(" (F{})", m.function_id))
                    .unwrap_or_default();
                println!("  Module {dep_id}{dep_info}");
            }
            if dependents.len() > 20 {
                println!("  ... and {} more", dependents.len() - 20);
            }
        }
    } else {
        println!("Module {module_id} not found in registry.");
        println!("\nRegistry contains {} modules.", registry.modules.len());
        println!("\nTip: Use 'hermes-dec modules <file>' to list all modules.");
    }

    Ok(())
}
