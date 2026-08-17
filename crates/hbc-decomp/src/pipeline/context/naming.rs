// Phase 3: module naming, closure resolution, export analysis, IPA.
use std::collections::BTreeMap;
use crate::file::BytecodeFile;
use crate::ir::Statement;
use crate::transforms;
use super::super::build_function_name_index;
use super::PipelineContext;

impl PipelineContext {
    pub(super) fn run_naming_pipeline(
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
        // Re-analyze slots from current IR, then apply Metro roles only on
        // factory functions so children inherit `require`/`dependencyMap`.
        // Nested helpers that *reuse* the same slot index drop the role via
        // prefer_local_over_inherited (avoids `let require = Symbol_iterator`).
        let t = std::time::Instant::now();
        if let Some(ctx) = closure_ctx.as_mut() {
            // First resolve: rebuild slot maps from IR that still has ClosureVar stores.
            Self::resolve_all_closures(all_ir, ctx, true, |ctx| {
                ctx.apply_metro_factory_param_roles(|id| {
                    registry.function_to_module.contains_key(&id)
                });
            });
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
            // Second resolve: do NOT reanalyze (would wipe env stores already turned
            // into Variables). Only refresh names on existing slot maps + resolve
            // any residual ClosureVar.
            Self::resolve_all_closures(all_ir, ctx, false, |ctx| {
                ctx.update_with_ipa_names(&global_analysis.param_names);
                ctx.apply_metro_factory_param_roles(|id| {
                    registry.function_to_module.contains_key(&id)
                });
            });
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
}
