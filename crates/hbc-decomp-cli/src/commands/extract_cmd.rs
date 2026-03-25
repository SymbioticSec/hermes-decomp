use hbc_decomp::{
    BytecodeFile, BytecodeFormat, DecompileOptionsV2, IRBuilder, IRBuilderOptions, MetroRegistry,
    StructureAnalysis,
};
use std::error::Error;
use std::fs;
use std::path::Path;

pub fn run_extract(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    output_dir: &Path,
    resolve_strings: bool,
) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(output_dir)?;
    println!("Extracting modules to {}...", output_dir.display());

    // 1. Analyze global function to find modules
    let options = IRBuilderOptions {
        resolve_strings: true,
        include_offsets: false,
        ..Default::default()
    };
    let mut builder = IRBuilder::new(file, format, options);
    let cfg = builder.build_function(0)?;
    let analysis = StructureAnalysis::analyze(&cfg);
    let statements = analysis.root.to_statements(&cfg);
    let registry = MetroRegistry::analyze(&statements);

    println!("Found {} modules.", registry.modules.len());

    // 2. Decompile each module
    let decompile_opts = DecompileOptionsV2 {
        resolve_strings,
        include_offsets: false,
        propagate: true,
        simplify: true,
        recover_structures: true,
        ..Default::default()
    };

    // Pre-calculate closure contexts
    let ctx = hbc_decomp::build_closure_context(file, format)?;

    for module in registry.modules.values() {
        let filename = if let Some(name) = &module.name {
            // Sanitize filename
            let safe_name = name.replace(['/', '\\'], "_");
            format!("{safe_name}.js")
        } else {
            format!("module_{}.js", module.module_id)
        };
        let path = output_dir.join(filename);

        print!(
            "Extracting module {} (F{})... ",
            module.module_id, module.function_id
        );

        match hbc_decomp::decompile_function_v2_with_context(
            file,
            format,
            module.function_id,
            &decompile_opts,
            Some(&ctx),
        ) {
            Ok(code) => {
                // Add header
                let mut content = String::new();
                content.push_str(&format!("// Module ID: {}\n", module.module_id));
                content.push_str(&format!("// Function ID: {}\n", module.function_id));
                if let Some(name) = &module.name {
                    content.push_str(&format!("// Name: {name}\n"));
                }
                content.push_str(&format!("// Dependencies: {:?}\n\n", module.dependencies));
                content.push_str(&code);

                fs::write(&path, content)?;
                println!("OK");
            }
            Err(e) => {
                println!("Error: {e}");
            }
        }
    }

    Ok(())
}

pub fn print_modules(
    file: &BytecodeFile,
    format: &hbc_decomp::BytecodeFormat,
    limit: Option<usize>,
) -> Result<(), Box<dyn Error>> {
    // The global function (F0) contains all module registrations
    let options = IRBuilderOptions {
        resolve_strings: true,
        include_offsets: false,
        ..Default::default()
    };
    let mut builder = IRBuilder::new(file, format, options);
    let cfg = builder.build_function(0)?;

    let analysis = StructureAnalysis::analyze(&cfg);
    let statements = analysis.root.to_statements(&cfg);

    // Build the Metro registry
    let registry = MetroRegistry::analyze(&statements);

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
        println!(
            "Module {} (F{}){}{}",
            module.module_id, module.function_id, name_str, deps_str
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
    module_id: u32,
    depth: usize,
) -> Result<(), Box<dyn Error>> {
    // The global function (F0) contains all module registrations
    let options = IRBuilderOptions {
        resolve_strings: true,
        include_offsets: false,
        ..Default::default()
    };
    let mut builder = IRBuilder::new(file, format, options);
    let cfg = builder.build_function(0)?;

    let analysis = StructureAnalysis::analyze(&cfg);
    let statements = analysis.root.to_statements(&cfg);

    // Build the Metro registry
    let registry = MetroRegistry::analyze(&statements);

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
