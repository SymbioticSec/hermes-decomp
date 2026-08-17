// Per-function codegen from the precomputed pipeline context.
use std::collections::BTreeMap;
use std::sync::Arc;
use crate::file::BytecodeFile;
use crate::ir::Statement;
use crate::transforms::{self, Codegen, CodegenOptions};
use super::super::{get_function_name, get_function_params};
use super::PipelineContext;

impl PipelineContext {
    // Resolve the module a function belongs to (directly or via parent closures).
    pub(super) fn resolve_module_for_function(&self, function_id: u32) -> Option<&crate::analysis::MetroModule> {
        // Direct module factory
        if let Some(&mod_id) = self.registry.function_to_module.get(&function_id) {
            return self.registry.modules.get(&mod_id);
        }
        // Traverse parent closures with cycle detection
        if let Some(ctx) = &self.closure_ctx {
            let mut visited = std::collections::HashSet::new();
            visited.insert(function_id);
            let mut current = function_id;
            while let Some(&parent) = ctx.parent_function.get(&current) {
                if !visited.insert(parent) {
                    break;
                }
                if let Some(&mod_id) = self.registry.function_to_module.get(&parent) {
                    return self.registry.modules.get(&mod_id);
                }
                current = parent;
            }
        }
        None
    }

    // Build import map (dep_module_id → name) for a module.
    pub(super) fn build_import_map(&self, module: &crate::analysis::MetroModule) -> BTreeMap<u32, String> {
        let mut imports = BTreeMap::new();
        for &dep_id in &module.dependencies {
            if let Some(dep_mod) = self.registry.modules.get(&dep_id) {
                if let Some(name) = &dep_mod.name {
                    imports.insert(dep_id, name.clone());
                }
            }
        }
        imports
    }

    // Write counts for free variables mutated in descendant closures of `function_id`.
    // Used so parent scopes emit `let` instead of `const` when children reassign.
    pub(super) fn extra_writes_for_function(&self, function_id: u32) -> BTreeMap<String, usize> {
        if self.closure_ctx.is_none() {
            return BTreeMap::new();
        }
        // parent -> children was inverted once at build time (self.child_functions).
        let children = &self.child_functions;
        // Collect all descendants (BFS)
        let mut nested_ids = Vec::new();
        let mut stack: Vec<u32> = children.get(&function_id).cloned().unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            nested_ids.push(id);
            if let Some(kids) = children.get(&id) {
                stack.extend(kids.iter().copied());
            }
        }
        let bodies: Vec<&[Statement]> = nested_ids
            .iter()
            .filter_map(|id| self.all_ir.get(id).map(|s| s.as_slice()))
            .collect();
        transforms::extra_writes_from_nested_bodies(&bodies)
    }

    // Generate decompiled code for a single function using cached analysis.
    pub fn generate_function_code(&self, file: &BytecodeFile, function_id: u32) -> String {
        // Reanimated worklet: emit its recovered original source.
        if let Some(src) = self.worklet_source_for(file, function_id) {
            return format!("{src}\n");
        }
        let Some(statements) = self.all_ir.get(&function_id) else {
            return format!("// Error: no IR for function {function_id}\n");
        };

        let mut statements = statements.clone();

        // Apply IPA parameter names to the IR
        if let Some(param_names) = self.global_analysis.param_names.get(&function_id) {
            transforms::exports::rename_param_registers(&mut statements, param_names);
        }

        // Lightweight cleanup after IPA renames (self-assignments, reserved words)
        statements = transforms::cleanup_noise(statements);
        transforms::rename_reserved_words(&mut statements);

        // Get function name
        let function_name = get_function_name(file, function_id);

        // Get params with IPA names
        let params = if let Some(names) = self.global_analysis.param_names.get(&function_id) {
            names
                .iter()
                .enumerate()
                .map(|(idx, n)| n.clone().unwrap_or_else(|| format!("arg{idx}")))
                .collect()
        } else {
            get_function_params(file, function_id)
        };

        // Resolve module context and build import map
        let module = self.resolve_module_for_function(function_id);
        let import_map = module.map(|m| self.build_import_map(m));

        // Use pre-built inline bodies for nested function rendering
        let codegen_options = CodegenOptions::default();
        let mut codegen = Codegen::new(codegen_options).with_inline_bodies(Arc::clone(&self.inline_bodies));
        if let Some(imports) = import_map {
            codegen = codegen.with_imports(imports);
        }

        // Check if this is a module factory (directly)
        let is_factory = self.registry.function_to_module.contains_key(&function_id);

        if is_factory {
            // Build dep_names (index→name) for ESM mode
            let module = match self.registry.get_module_for_function(function_id) {
                Some(m) => m,
                None => {
                    // Registry inconsistency: function_to_module contains key but get_module_for_function returns None
                    return format!("// Error: module not found for function {function_id}\n");
                }
            };
            let mut dep_names = BTreeMap::new();
            let mut dep_ids = BTreeMap::new();
            for (idx, &dep_id) in module.dependencies.iter().enumerate() {
                dep_ids.insert(idx as u32, dep_id);
                if let Some(dep_mod) = self.registry.modules.get(&dep_id) {
                    if let Some(name) = &dep_mod.name {
                        dep_names.insert(idx as u32, name.clone());
                    } else {
                        dep_names.insert(idx as u32, format!("module_{dep_id}"));
                    }
                }
            }
            codegen = codegen
                .with_esm_mode(dep_names)
                .with_esm_module_meta(dep_ids);
            let extra = self.extra_writes_for_function(function_id);
            transforms::insert_declarations_with_extra_writes(&mut statements, &params, &extra);
            codegen.generate_esm_module(
                &statements,
                module.module_id,
                module.name.as_deref(),
            )
        } else {
            // Insert const/let declarations into the IR before codegen.
            // Nested functions must not redeclare ancestor env slots.
            let extra = self.extra_writes_for_function(function_id);
            let outer = self.ancestor_env_slot_names(function_id);
            transforms::insert_declarations_with_outer(
                &mut statements,
                &params,
                &extra,
                &outer,
                true,
            );

            let body = codegen.generate_statements(&statements);

            let is_async = self.closure_ctx.as_ref().is_some_and(|c| c.is_async(function_id));
            let is_generator = self.closure_ctx.as_ref().is_some_and(|c| c.is_generator(function_id));
            // Async generators (Babel pattern) render as async, not function*
            let is_generator = is_generator && !is_async;
            let async_prefix = if is_async { "async " } else { "" };
            let gen_star = if is_generator { "*" } else { "" };
            let params_str = params.join(", ");

            let mut output = String::new();
            output.push_str(&format!(
                "{async_prefix}function{gen_star} {function_name}({params_str}) {{\n"
            ));

            for line in body.lines() {
                output.push_str("  ");
                output.push_str(line);
                output.push('\n');
            }
            output.push_str("}\n");
            output
        }
    }
}
