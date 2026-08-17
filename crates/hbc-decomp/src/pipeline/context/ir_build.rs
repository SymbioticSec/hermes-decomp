// Metro registry + parallel optimized IR generation.
use std::collections::BTreeMap;
use crate::file::BytecodeFile;
use crate::ir::Statement;
use crate::opcode::BytecodeFormat;
use crate::transforms;
use super::super::{apply_register_naming, generate_ir, DecompileOptionsV2};
use super::PipelineContext;

impl PipelineContext {
    pub(super) fn build_metro_registry(file: &BytecodeFile, format: &BytecodeFormat) -> crate::analysis::MetroRegistry {
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
    pub(super) fn generate_all_optimized_ir(
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
                    let named = apply_register_naming(stmts, file, i);
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
}
