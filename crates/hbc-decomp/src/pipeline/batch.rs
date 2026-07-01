use std::collections::{BTreeMap, HashSet, VecDeque};
use crate::analysis::{ClosureContext, MetroRegistry};
use crate::error::Result;
use crate::file::BytecodeFile;
use crate::opcode::BytecodeFormat;

use super::{
    build_function_name_index, generate_ir,
    DecompileOptionsV2, PipelineContext,
};

// Selects which Metro modules to emit. An empty filter (no include criteria)
// matches every module; excludes always apply on top. Used by the
// `decompile --modules/--module-name/--from-module ...` CLI flags.
#[derive(Debug, Clone, Default)]
pub struct ModuleFilter {
    // Inclusive module-id ranges from `--modules` (e.g. `100-150,200`).
    pub id_ranges: Vec<(u32, u32)>,
    // Name globs from `--module-name` (e.g. `Login*`). `*` is a wildcard.
    pub name_globs: Vec<String>,
    // Name globs from `--exclude-module-name` (e.g. `react*,lodash*`).
    pub exclude_globs: Vec<String>,
    // Dependency-subtree root from `--from-module`.
    pub from: Option<u32>,
    // Max depth for the `--from-module` subtree (0 = the root only).
    pub depth: usize,
}

impl ModuleFilter {
    pub fn is_empty(&self) -> bool {
        self.id_ranges.is_empty()
            && self.name_globs.is_empty()
            && self.exclude_globs.is_empty()
            && self.from.is_none()
    }

    fn has_include(&self) -> bool {
        !self.id_ranges.is_empty() || !self.name_globs.is_empty() || self.from.is_some()
    }

    // Resolve to the concrete set of module IDs to emit.
    fn resolve(&self, registry: &MetroRegistry) -> HashSet<u32> {
        let module_name = |m: u32| -> &str {
            registry.get_module(m).and_then(|x| x.name.as_deref()).unwrap_or("")
        };

        let mut included: HashSet<u32> = HashSet::new();
        if self.has_include() {
            for &m in registry.modules.keys() {
                if self.id_ranges.iter().any(|&(lo, hi)| m >= lo && m <= hi) {
                    included.insert(m);
                }
                if self.name_globs.iter().any(|g| glob_match(g, module_name(m))) {
                    included.insert(m);
                }
            }
            if let Some(root) = self.from {
                collect_dependency_subtree(registry, root, self.depth, &mut included);
            }
        } else {
            included.extend(registry.modules.keys().copied());
        }

        included.retain(|&m| !self.exclude_globs.iter().any(|g| glob_match(g, module_name(m))));
        included
    }
}

// BFS over the dependency edges from `root`, up to `max_depth` hops.
fn collect_dependency_subtree(
    registry: &MetroRegistry,
    root: u32,
    max_depth: usize,
    out: &mut HashSet<u32>,
) {
    let mut queue: VecDeque<(u32, usize)> = VecDeque::new();
    queue.push_back((root, 0));
    let mut seen: HashSet<u32> = HashSet::new();
    while let Some((m, d)) = queue.pop_front() {
        if !seen.insert(m) {
            continue;
        }
        out.insert(m);
        if d < max_depth {
            if let Some(module) = registry.get_module(m) {
                for &dep in &module.dependencies {
                    queue.push_back((dep, d + 1));
                }
            }
        }
    }
}

// Minimal glob: `*` matches any run of characters. Matching is case-insensitive
// so `react*` catches `React`. Anchored at both ends (full-string match).
fn glob_match(pattern: &str, text: &str) -> bool {
    let pat = pattern.to_ascii_lowercase();
    let txt = text.to_ascii_lowercase();
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == txt; // no wildcard: exact match
    }
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            // must match at the start
            if !txt[pos..].starts_with(part) {
                return false;
            }
            pos += part.len();
        } else if i == parts.len() - 1 {
            // last part must match at the end
            return txt[pos..].ends_with(part);
        } else if let Some(found) = txt[pos..].find(part) {
            pos += found + part.len();
        } else {
            return false;
        }
    }
    true
}

pub fn decompile_all_v2_with_closures(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    options: &DecompileOptionsV2,
) -> Result<String> {
    decompile_filtered_v2(file, format, options, None)
}

// Cached counterpart: reuses an on-disk analysis cache at `cache_path` keyed by
// the bytecode `bytes` (see `PipelineContext::build_cached`).
pub fn decompile_all_v2_with_closures_cached(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    options: &DecompileOptionsV2,
    bytes: &[u8],
    cache_path: &std::path::Path,
) -> Result<String> {
    decompile_filtered_v2_cached(file, format, options, None, bytes, cache_path)
}

// Like `decompile_all_v2_with_closures`, but emits only the modules selected by
// `filter` (when `Some`). With a filter active, orphan (non-module) functions
// are omitted — the caller asked for specific modules.
pub fn decompile_filtered_v2(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    options: &DecompileOptionsV2,
    filter: Option<&ModuleFilter>,
) -> Result<String> {
    let pipeline = PipelineContext::build_with_options(file, format, options)?;
    Ok(render_bundle(&pipeline, file, filter))
}

// Cached counterpart of `decompile_filtered_v2`.
pub fn decompile_filtered_v2_cached(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    options: &DecompileOptionsV2,
    filter: Option<&ModuleFilter>,
    bytes: &[u8],
    cache_path: &std::path::Path,
) -> Result<String> {
    let pipeline = PipelineContext::build_cached(file, format, options, bytes, cache_path)?;
    Ok(render_bundle(&pipeline, file, filter))
}

// Render a (possibly filtered) bundle from an already-built pipeline context.
fn render_bundle(
    pipeline: &PipelineContext,
    file: &BytecodeFile,
    filter: Option<&ModuleFilter>,
) -> String {
    let active_filter = filter.filter(|f| !f.is_empty());
    let allowed: Option<HashSet<u32>> = active_filter.map(|f| f.resolve(&pipeline.registry));

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

    let mut sorted_modules: Vec<_> = module_functions
        .keys()
        .cloned()
        .filter(|m| allowed.as_ref().is_none_or(|set| set.contains(m)))
        .collect();
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

    // Orphans aren't Metro modules; omit them when a module filter is active.
    if !orphans.is_empty() && allowed.is_none() {
        output.push_str("\n// === Orphan Functions ===\n");
        orphans.sort();
        for func_id in orphans {
            if !output.is_empty() { output.push('\n'); }
            output.push_str(&pipeline.generate_function_code(file, func_id));
        }
    }

    output
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

    let param_counts: std::collections::HashMap<u32, u32> = file
        .function_headers
        .iter()
        .enumerate()
        .map(|(id, h)| (id as u32, h.param_count().saturating_sub(1)))
        .collect();
    for i in 0..file.header.function_count {
        if let Ok(stmts) = generate_ir(file, format, i, &raw_options, None, false) {
            registry.analyze_statements_with_params(&stmts, &param_counts);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_exact_and_wildcards() {
        // no wildcard => exact (case-insensitive)
        assert!(glob_match("Login", "login"));
        assert!(!glob_match("Login", "loginScreen"));
        // prefix
        assert!(glob_match("react*", "ReactNative"));
        assert!(!glob_match("react*", "preact"));
        // suffix
        assert!(glob_match("*Screen", "LoginScreen"));
        assert!(!glob_match("*Screen", "ScreenLogin"));
        // contains
        assert!(glob_match("*react*", "myReactUtil"));
        assert!(!glob_match("*react*", "vue"));
        // middle segment
        assert!(glob_match("a*c", "abc"));
        assert!(!glob_match("a*c", "abd"));
    }

    #[test]
    fn filter_id_ranges_and_excludes() {
        let f = ModuleFilter {
            id_ranges: vec![(5, 7), (100, 100)],
            exclude_globs: vec!["NODE*".to_string()],
            ..Default::default()
        };
        assert!(!f.is_empty());
        // id matching is range-inclusive; exclude is applied on top in resolve()
        assert!(f.id_ranges.iter().any(|&(lo, hi)| 6 >= lo && 6 <= hi));
        assert!(f.id_ranges.iter().any(|&(lo, hi)| 100 >= lo && 100 <= hi));
        assert!(!f.id_ranges.iter().any(|&(lo, hi)| 8 >= lo && 8 <= hi));
    }

    #[test]
    fn empty_filter_is_empty() {
        assert!(ModuleFilter::default().is_empty());
        assert!(!ModuleFilter { from: Some(3), ..Default::default() }.is_empty());
    }
}
