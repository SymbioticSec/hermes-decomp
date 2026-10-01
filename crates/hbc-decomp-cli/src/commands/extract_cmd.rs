use hbc_decomp::{
    BytecodeFile, BytecodeFormat, DecompileOptionsV2, DependencyTree, MetroRegistry,
    PipelineContext,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
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
    json: bool,
) -> Result<(), Box<dyn Error>> {
    // Use the full analysis pipeline so module names and exports are resolved
    // (the lightweight detection registry only knows IDs and dependencies).
    let ctx = build_cached_pipeline(file, format, bytes, cache_path)?;
    let registry = &ctx.registry;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&modules_json(registry, limit))?
        );
        return Ok(());
    }

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
    json: bool,
) -> Result<(), Box<dyn Error>> {
    // Full pipeline so dependency names (not just IDs) are available.
    let ctx = build_cached_pipeline(file, format, bytes, cache_path)?;
    let registry = &ctx.registry;

    if json {
        let doc = deps_json(registry, module_id, depth)?;
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }

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

// The module listing as a JSON document: the first `limit` modules in id order
// plus the total count, each with its dependencies and export map.
pub fn modules_json(registry: &MetroRegistry, limit: Option<usize>) -> Value {
    let modules: Vec<Value> = registry
        .modules
        .values()
        .take(limit.unwrap_or(usize::MAX))
        .map(|m| {
            let exports: BTreeMap<&String, &u32> = m.exports.iter().collect();
            json!({
                "module_id": m.module_id,
                "function_id": m.function_id,
                "name": m.name,
                "dependencies": m.dependencies,
                "exports": exports,
            })
        })
        .collect();
    json!({ "total": registry.modules.len(), "modules": modules })
}

fn tree_json(tree: &DependencyTree) -> Value {
    json!({
        "module_id": tree.module_id,
        "function_id": tree.function_id,
        "name": tree.name,
        "children": tree.children.iter().map(tree_json).collect::<Vec<_>>(),
    })
}

// One module's dependencies, dependency tree and dependents as a JSON document.
// An unknown module is an error here: a machine consumer needs a clear failure.
pub fn deps_json(
    registry: &MetroRegistry,
    module_id: u32,
    depth: usize,
) -> Result<Value, Box<dyn Error>> {
    let module = registry.get_module(module_id).ok_or_else(|| {
        format!(
            "module {module_id} not found in registry ({} modules)",
            registry.modules.len()
        )
    })?;
    let with_function = |id: u32| json!({ "module_id": id, "function_id": registry.get_module(id).map(|m| m.function_id) });
    let dependencies: Vec<Value> = module
        .dependencies
        .iter()
        .map(|&d| with_function(d))
        .collect();
    let dependents: Vec<Value> = registry
        .get_dependents(module_id)
        .into_iter()
        .map(with_function)
        .collect();
    Ok(json!({
        "module_id": module.module_id,
        "function_id": module.function_id,
        "name": module.name,
        "dependencies": dependencies,
        "tree": tree_json(&registry.get_dependency_tree(module_id, depth)),
        "dependents": dependents,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hbc_decomp::MetroModule;
    use std::collections::HashMap;

    fn registry() -> MetroRegistry {
        let module = |id: u32, name: &str, deps: Vec<u32>, exports: Vec<(&str, u32)>| MetroModule {
            module_id: id,
            function_id: id * 10,
            name: Some(name.to_string()),
            dependencies: deps,
            exports: exports
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect::<HashMap<_, _>>(),
            roles: Default::default(),
            name_from_default_export: false,
        };
        let mut registry = MetroRegistry::new();
        for m in [
            module(0, "app", vec![1, 2], vec![("default", 1)]),
            module(1, "utils", vec![2], vec![("format", 11), ("parse", 12)]),
            module(2, "core", vec![], vec![]),
        ] {
            registry
                .function_to_module
                .insert(m.function_id, m.module_id);
            registry.modules.insert(m.module_id, m);
        }
        registry
    }

    #[test]
    fn modules_json_lists_modules_with_limit() {
        let doc = modules_json(&registry(), Some(2));
        let back: Value = serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert_eq!(back["total"], 3);
        let modules = back["modules"].as_array().unwrap();
        assert_eq!(modules.len(), 2);
        assert_eq!(modules[0]["module_id"], 0);
        assert_eq!(modules[0]["name"], "app");
        assert_eq!(modules[1]["exports"]["format"], 11);
        assert_eq!(modules[1]["dependencies"], json!([2]));
    }

    #[test]
    fn deps_json_reports_tree_and_dependents() {
        let doc = deps_json(&registry(), 1, 2).unwrap();
        let back: Value = serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert_eq!(back["module_id"], 1);
        assert_eq!(back["function_id"], 10);
        assert_eq!(back["dependencies"][0]["module_id"], 2);
        assert_eq!(back["dependencies"][0]["function_id"], 20);
        assert_eq!(back["tree"]["children"][0]["module_id"], 2);
        assert_eq!(back["dependents"][0]["module_id"], 0);
        assert!(deps_json(&registry(), 42, 1).is_err());
    }
}
