// PipelineContext: pre-computed analysis context for efficient code generation.
// Built once (expensive), then used to generate code for individual functions cheaply.

mod async_detection;
mod rendering;

use std::collections::BTreeMap;
use std::sync::Arc;
use crate::analysis::ClosureContext;
use crate::error::Result;
use crate::file::BytecodeFile;
use crate::ir::Statement;
use crate::opcode::BytecodeFormat;
use crate::transforms::{self, Codegen, CodegenOptions};

use super::{
    build_closure_context_from_file, generate_ir, get_function_name, get_function_params,
    build_function_name_index, DecompileOptionsV2,
};
use super::ir_gen::convert_yields_to_awaits;

// Pre-computed pipeline context that holds all intermediate analysis results.
// Built once (expensive), then used to generate code for individual functions cheaply.
pub struct PipelineContext {
    pub all_ir: BTreeMap<u32, Vec<Statement>>,
    pub registry: crate::analysis::MetroRegistry,
    pub closure_ctx: Option<ClosureContext>,
    pub global_analysis: crate::analysis::GlobalAnalysis,
    // Pre-rendered inline function bodies (function_id → complete function expression string).
    // Built once after all IR is generated, supports multi-level nesting.
    pub(super) inline_bodies: Arc<BTreeMap<u32, String>>,
    // Recovered Reanimated worklet sources (function name → original source),
    // extracted from `__initData.code` string constants in the bundle.
    pub(super) worklet_sources: BTreeMap<String, String>,
}

impl PipelineContext {
    // Run the full analysis pipeline. This is expensive (processes all functions).
    pub fn build(file: &BytecodeFile, format: &BytecodeFormat) -> Result<Self> {
        Self::build_with_options(file, format, &DecompileOptionsV2::optimized())
    }

    // Run the full analysis pipeline with user-provided options.
    pub fn build_with_options(file: &BytecodeFile, format: &BytecodeFormat, user_options: &DecompileOptionsV2) -> Result<Self> {
        crate::configure_thread_pool();

        let total_start = std::time::Instant::now();
        let options = DecompileOptionsV2 {
            assembly_mode: user_options.assembly_mode,
            include_offsets: user_options.include_offsets || user_options.assembly_mode,
            ..DecompileOptionsV2::optimized()
        };

        // STAGE W1: Closure Context Build
        let t = std::time::Instant::now();
        let mut closure_ctx = Some(build_closure_context_from_file(file, format)?);
        log::debug!("[pipeline] closure context: {:.2?}", t.elapsed());

        // STAGE W2: Metro Detection
        let mut registry = Self::build_metro_registry(file, format);

        // STAGE W3-W4: Generate optimized IR (parallel) + closure analysis
        let mut all_ir = Self::generate_all_optimized_ir(file, format, &options, &mut closure_ctx);

        // STAGE W5-W11: Name resolution (module names, closures, exports, IPA)
        let mut global_analysis = Self::run_naming_pipeline(
            &mut all_ir, &mut registry, &mut closure_ctx, file,
        );

        // STAGE W12-W16: Transform pipeline (inlining, async detection, post-IPA)
        Self::run_transform_pipeline(
            &mut all_ir, &mut closure_ctx, &mut global_analysis, file,
        );

        // Recover original worklet sources from embedded `__initData.code` strings.
        let worklet_sources = transforms::collect_worklet_sources(&all_ir);
        log::debug!("[pipeline] recovered {} worklet sources", worklet_sources.len());

        // STAGE W17: Inline body rendering
        let mut ctx = PipelineContext {
            all_ir,
            registry,
            closure_ctx,
            global_analysis,
            inline_bodies: Arc::new(BTreeMap::new()),
            worklet_sources,
        };

        let t = std::time::Instant::now();
        ctx.build_all_inline_bodies(file);
        log::debug!("[pipeline] inline body rendering: {:.2?} ({} of {} functions)", t.elapsed(), ctx.inline_bodies.len(), file.header.function_count);

        log::debug!("[pipeline] exception handlers: {} functions with try/catch", file.exception_handlers.len());
        log::debug!("[pipeline] TOTAL: {:.2?}", total_start.elapsed());

        Ok(ctx)
    }


    // Phase 1b: Detect Metro modules by scanning the global function with minimal IR.
    fn build_metro_registry(file: &BytecodeFile, format: &BytecodeFormat) -> crate::analysis::MetroRegistry {
        let t = std::time::Instant::now();
        let raw_options = DecompileOptionsV2 {
            resolve_strings: true,
            ..DecompileOptionsV2::default()
        };

        let mut registry = crate::analysis::MetroRegistry::new();
        let global_idx = file.header.global_code_index;
        // function_id -> declared parameter count (this-excluded), so Metro
        // factory roles are derived from the real arity (4-param classic vs
        // 7-param modern with importDefault/importAll).
        let param_counts: std::collections::HashMap<u32, u32> = file
            .function_headers
            .iter()
            .enumerate()
            .map(|(id, h)| (id as u32, h.param_count().saturating_sub(1)))
            .collect();
        if let Ok(stmts) = generate_ir(file, format, global_idx, &raw_options, None, false) {
            registry.analyze_statements_with_params(&stmts, &param_counts);
        }
        log::debug!("[pipeline] metro detection: {:.2?} ({} modules)", t.elapsed(), registry.modules.len());
        registry
    }

    // Phase 2: Generate optimized IR in parallel, then run closure analysis sequentially.
    fn generate_all_optimized_ir(
        file: &BytecodeFile,
        format: &BytecodeFormat,
        options: &DecompileOptionsV2,
        closure_ctx: &mut Option<crate::analysis::ClosureContext>,
    ) -> BTreeMap<u32, Vec<Statement>> {
        use rayon::prelude::*;

        let t = std::time::Instant::now();
        let named_irs: Vec<Option<(u32, Vec<Statement>)>> = {
            let ctx_ref = closure_ctx.as_ref();
            (0..file.header.function_count)
                .into_par_iter()
                .map(|i| {
                    let stmts = generate_ir(file, format, i, options, ctx_ref, false)
                        .map_err(|e| log::debug!("[pipeline] IR gen failed for func {i}: {e}"))
                        .ok()?;
                    let named = super::apply_register_naming(stmts, file, i);
                    let semantic = transforms::infer_variable_names(named);
                    let mut final_stmts = semantic;
                    crate::transforms::simplify_statements(&mut final_stmts);
                    Some((i, final_stmts))
                })
                .collect()
        };
        log::debug!("[pipeline] optimized IR generation (parallel): {:.2?}", t.elapsed());

        let t = std::time::Instant::now();
        let mut all_ir = BTreeMap::new();
        for item in named_irs.into_iter().flatten() {
            let (i, final_stmts) = item;
            if let Some(ctx) = closure_ctx.as_mut() {
                ctx.analyze_function(i, &final_stmts);
            }
            all_ir.insert(i, final_stmts);
        }
        if let Some(ctx) = closure_ctx.as_mut() {
            ctx.propagate_async_to_generators();
        }
        log::debug!("[pipeline] closure analyze + insert: {:.2?}", t.elapsed());
        all_ir
    }

    // Phase 3: Module naming, closure resolution, export analysis, and IPA.
    fn run_naming_pipeline(
        all_ir: &mut BTreeMap<u32, Vec<Statement>>,
        registry: &mut crate::analysis::MetroRegistry,
        closure_ctx: &mut Option<crate::analysis::ClosureContext>,
        file: &BytecodeFile,
    ) -> crate::analysis::GlobalAnalysis {
        // STAGE W5: Module Name Propagation
        let t = std::time::Instant::now();
        crate::analysis::metro::propagate_module_names(all_ir, registry, closure_ctx);
        log::debug!("[pipeline] module name propagation: {:.2?}", t.elapsed());

        // STAGE W6: Closure Resolution (first pass)
        // Apply Metro roles only on factory functions' slot maps (shared so
        // children can inherit `require`/`dependencyMap` for true captures).
        // Nested helpers that *reuse* the same slot index drop the role via
        // `prefer_local_over_inherited` (avoids `let require = Symbol_iterator`).
        let t = std::time::Instant::now();
        if let Some(ctx) = closure_ctx.as_mut() {
            ctx.apply_metro_factory_param_roles(|id| registry.function_to_module.contains_key(&id));
            Self::resolve_all_closures(all_ir, ctx);
        }
        log::debug!("[pipeline] closure resolution: {:.2?}", t.elapsed());

        // STAGE W7: Metro Export Analysis
        let t = std::time::Instant::now();
        let mut export_mod_ids: Vec<_> = registry.modules.keys().copied().collect();
        export_mod_ids.sort();
        for mid in export_mod_ids {
            if let Some(module) = registry.modules.get_mut(&mid) {
                crate::analysis::metro::exports::ExportAnalyzer::analyze(module, all_ir);
            }
        }
        log::debug!("[pipeline] metro export analysis: {:.2?}", t.elapsed());

        // STAGE W8: Inter-Procedural Analysis (IPA)
        let t = std::time::Instant::now();
        let func_name_index = build_function_name_index(file);
        let global_analysis = crate::analysis::run_ipa(all_ir, registry, &func_name_index);
        log::debug!("[pipeline] IPA: {:.2?}", t.elapsed());

        // STAGE W9: IPA Closure Re-resolve
        let t = std::time::Instant::now();
        if let Some(ctx) = closure_ctx.as_mut() {
            ctx.update_with_ipa_names(&global_analysis.param_names);
            ctx.apply_metro_factory_param_roles(|id| registry.function_to_module.contains_key(&id));
            Self::resolve_all_closures(all_ir, ctx);
        }
        log::debug!("[pipeline] IPA closure re-resolve: {:.2?}", t.elapsed());

        // STAGE W10: Closure Property Naming (cross-function)
        let t = std::time::Instant::now();
        let closure_renames = if let Some(ctx) = closure_ctx.as_ref() {
            transforms::rename_closure_variables_cross_function(all_ir, ctx)
        } else {
            let mut count = 0;
            let mut fb_keys: Vec<_> = all_ir.keys().copied().collect();
            fb_keys.sort();
            for key in fb_keys {
                if let Some(stmts) = all_ir.get_mut(&key) {
                    count += transforms::rename_closure_variables(stmts);
                }
            }
            count
        };
        log::debug!("[pipeline] closure property naming: {:.2?} ({closure_renames} variables renamed)", t.elapsed());

        // STAGE W11: Definition-site closure naming
        let def_renames = transforms::rename_closures_from_definitions(all_ir);
        if def_renames > 0 {
            log::debug!("[pipeline] closure definition naming: {def_renames} variables renamed");
        }

        // STAGE W12: dependencyMap[N] → absolute module IDs.
        // After resolve_closures AND closure naming: heavily-indexed captures are
        // renamed to `dependencyMap` / `dependencyMap2` only in W10, so this must
        // run last among the naming stages.
        let t = std::time::Instant::now();
        crate::analysis::metro::rewrite_dependency_maps_late(all_ir, registry, closure_ctx);
        log::debug!("[pipeline] dependencyMap rewrite (post-naming): {:.2?}", t.elapsed());

        global_analysis
    }

    // Phase 4: Strip this, inline temporaries, detect async patterns, post-IPA transforms.
    fn run_transform_pipeline(
        all_ir: &mut BTreeMap<u32, Vec<Statement>>,
        closure_ctx: &mut Option<crate::analysis::ClosureContext>,
        global_analysis: &mut crate::analysis::GlobalAnalysis,
        file: &BytecodeFile,
    ) {
        // STAGE W12: Strip meaningless Hermes `this` from Call expressions
        for stmts in all_ir.values_mut() {
            transforms::strip_hermes_this(stmts);
        }

        // STAGE W13: Inline single-use temporaries (tmp*, closure_*, rN), parallel.
        let t = std::time::Instant::now();
        {
            use rayon::prelude::*;
            let keys: Vec<u32> = all_ir.keys().copied().collect();
            let mut entries: Vec<(u32, Vec<Statement>)> = keys
                .into_iter()
                .filter_map(|id| all_ir.remove(&id).map(|s| (id, s)))
                .collect();
            entries.par_iter_mut().for_each(|(_, stmts)| {
                let old = std::mem::take(stmts);
                *stmts = transforms::inline_named_variables(old);
            });
            for (id, stmts) in entries {
                all_ir.insert(id, stmts);
            }
        }
        log::debug!("[pipeline] variable inlining: {:.2?}", t.elapsed());

        // STAGE W14: Detect async generator patterns (yield → await)
        if let Some(ctx) = closure_ctx.as_mut() {
            let async_gen_ids = async_detection::detect_async_generator_wrappers(all_ir);
            for func_id in &async_gen_ids {
                ctx.mark_async(*func_id);
            }
            if !async_gen_ids.is_empty() {
                for func_id in &async_gen_ids {
                    if let Some(stmts) = all_ir.get_mut(func_id) {
                        let old = std::mem::take(stmts);
                        *stmts = convert_yields_to_awaits(old);
                    }
                }
                log::debug!("[pipeline] async detection: {} functions converted yield→await", async_gen_ids.len());
            }
        }

        // STAGE W15: Unwrap Babel async wrappers
        if let Some(ctx) = closure_ctx.as_mut() {
            let unwrapped = async_detection::unwrap_async_wrappers(all_ir, ctx, &mut global_analysis.param_names, file);
            if unwrapped > 0 {
                log::debug!("[pipeline] async wrapper unwrap: {unwrapped} functions unwrapped");
            }
        }

        // STAGE W16: Post-IPA transforms (reserved words, object/array folding, arguments simplification)
        Self::apply_post_ipa_transforms(all_ir);

        // STAGE W16a: promote let→const where never reassigned
        for stmts in all_ir.values_mut() {
            transforms::promote_const_bindings(stmts);
        }

        // STAGE W16b: Collapse generator wrappers. A `function* gen()` compiles to
        // a thin wrapper that does `CreateGenerator(body); return it`, with the
        // actual state machine (the yields) in a separate inner function. Inline
        // the inner body into the wrapper so we emit `function* gen() { yield ... }`
        // instead of `function* gen() { return function*() { yield ... } }`.
        if let Some(ctx) = closure_ctx.as_mut() {
            Self::collapse_generator_wrappers(all_ir, ctx);
        }
    }

    // See STAGE W16b. Replace each generator wrapper's body with the inner
    // generator body it merely creates and returns.
    fn collapse_generator_wrappers(
        all_ir: &mut BTreeMap<u32, Vec<Statement>>,
        closure_ctx: &mut ClosureContext,
    ) {
        // A wrapper is any function whose body merely returns a generator closure
        // (`return function*(){...}`, after env-slot init). The wrapper itself is
        // NOT flagged is_generator (HBC marks the inner driver); detecting it by
        // shape, not by the flag, is what lets us collapse it.
        let mut replacements: Vec<(u32, u32)> = Vec::new();
        for (&fid, body) in all_ir.iter() {
            if let Some(inner) = generator_wrapper_target(body) {
                if inner != fid
                    && all_ir.contains_key(&inner)
                    && closure_ctx.is_generator(inner)
                {
                    replacements.push((fid, inner));
                }
            }
        }
        for (fid, inner) in replacements {
            if let Some(inner_body) = all_ir.get(&inner).cloned() {
                all_ir.insert(fid, inner_body);
                // The wrapper now IS the generator: flag it so codegen emits
                // `function*` and the state-machine reconstruction below runs on it.
                closure_ctx.mark_generator(fid);
                // The inner body now lives in the wrapper; drop the standalone copy
                // so it is not also emitted as an orphan function.
                all_ir.remove(&inner);
            }
        }
        // STAGE W16c: Reconstruct HBC >=97 generator state machines into flat
        // `yield` bodies. v97 removed the generator opcodes; `function*` is now a
        // desugared switch over status/label env slots. The recognizer is
        // conservative, it returns the body unchanged on any shape mismatch.
        let gen_ids: Vec<u32> = all_ir
            .keys()
            .copied()
            .filter(|fid| closure_ctx.is_generator(*fid))
            .collect();
        for fid in gen_ids {
            if let Some(body) = all_ir.remove(&fid) {
                all_ir.insert(fid, transforms::reconstruct_generator_v98(body));
            }
        }

        // STAGE W16d: Reconstruct HBC >=97 array destructuring from the flat
        // iterator protocol (after the cleanup-handler skip un-nests it). The
        // matcher is conservative, it only rewrites a recognized `iter =
        // src[Symbol.iterator](); ...advances/binds...; iter.return()` block.
        let fids: Vec<u32> = all_ir.keys().copied().collect();
        for fid in &fids {
            if let Some(body) = all_ir.remove(fid) {
                all_ir.insert(*fid, transforms::reconstruct_v98_array_destructuring(body));
            }
        }

        // STAGE W16e: JSX reconstruction on the fully-assembled, named IR. The
        // in-pipeline pass (F10) runs before object-literal reconstruction, so it
        // misses calls whose props object is materialized later; rerun here where
        // `jsx(Tag, {props, children})` is complete.
        for fid in &fids {
            if let Some(body) = all_ir.remove(fid) {
                all_ir.insert(*fid, transforms::reconstruct_jsx(body));
            }
        }
    }

    // Apply closure resolution to all functions using the given closure context.
    fn resolve_all_closures(all_ir: &mut BTreeMap<u32, Vec<Statement>>, closure_ctx: &ClosureContext) {
        let mut keys: Vec<_> = all_ir.keys().copied().collect();
        keys.sort();
        for i in keys {
            let closure_info = closure_ctx.get_closure_info_for(i);
            if !closure_info.slots.is_empty() {
                if let Some(stmts) = all_ir.get_mut(&i) {
                    let old = std::mem::take(stmts);
                    *stmts = crate::analysis::resolve_closures(old, &closure_info);
                }
            }
        }
    }

    // Apply post-IPA transforms: reserved words, object/array folding, arguments simplification.
    fn apply_post_ipa_transforms(all_ir: &mut BTreeMap<u32, Vec<Statement>>) {
        // Rename reserved JS keywords used as variable names (default → _default)
        for stmts in all_ir.values_mut() {
            transforms::rename_reserved_words(stmts);
        }

        // Fold incremental object/array construction into literals
        for stmts in all_ir.values_mut() {
            let old = std::mem::take(stmts);
            *stmts = transforms::fold_object_literals(old);
            let old = std::mem::take(stmts);
            *stmts = transforms::fold_array_literals(old);
        }

        // Simplify Babel arguments-to-array copy pattern
        for stmts in all_ir.values_mut() {
            let old = std::mem::take(stmts);
            *stmts = transforms::simplify_arguments_copy(old);
        }
    }

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
            for (idx, &dep_id) in module.dependencies.iter().enumerate() {
                if let Some(dep_mod) = self.registry.modules.get(&dep_id) {
                    if let Some(name) = &dep_mod.name {
                        dep_names.insert(idx as u32, name.clone());
                    } else {
                        dep_names.insert(idx as u32, format!("module_{dep_id}"));
                    }
                }
            }
            codegen = codegen.with_esm_mode(dep_names);
            transforms::insert_declarations(&mut statements, &params);
            codegen.generate_esm_module(
                &statements,
                module.module_id,
                module.name.as_deref(),
            )
        } else {
            // Insert const/let declarations into the IR before codegen
            transforms::insert_declarations(&mut statements, &params);

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

// If `body` is a thin generator wrapper that just creates and returns an inner
// generator closure, return that inner function id. Matches both the inlined
// form `return function*<X>()` and the two-statement `r = function*<X>(); return r`.
fn generator_wrapper_target(body: &[Statement]) -> Option<u32> {
    use crate::ir::{AssignTarget, Expression, Value};

    // Skip comments and the generator's env-slot initializers (`let closure_N =
    // 0;` / `closure_N = 0;`) that a v98 wrapper emits before returning the inner
    // generator, they are dead once the inner body is inlined.
    let is_env_init = |s: &Statement| -> bool {
        match s {
            Statement::Let { name, .. } => name.starts_with("closure_"),
            Statement::Assign { target: AssignTarget::ClosureVar { .. }, .. } => true,
            Statement::Assign { target: AssignTarget::Variable(n), .. } => n.starts_with("closure_"),
            _ => false,
        }
    };
    let meaningful: Vec<&Statement> = body
        .iter()
        .filter(|s| !matches!(s, Statement::Comment(_)) && !is_env_init(s))
        .collect();

    let inner_gen_id = |e: &Expression| -> Option<u32> {
        match e {
            Expression::Function { id, is_generator: true, .. } => Some(id.0),
            _ => None,
        }
    };

    match meaningful.as_slice() {
        // return function*() { ... }
        [Statement::Return(Some(e))] => inner_gen_id(e),
        // r = function*() { ... }; return r
        [Statement::Assign { target: AssignTarget::Register(r), value }, Statement::Return(Some(Expression::Value(Value::Register(rr))))]
            if r == rr =>
        {
            inner_gen_id(value)
        }
        _ => None,
    }
}
