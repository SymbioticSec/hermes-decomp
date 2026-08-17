// Transform pipeline stages (inline, async, generator collapse, folding).
use std::collections::BTreeMap;
use crate::analysis::ClosureContext;
use crate::file::BytecodeFile;
use crate::ir::Statement;
use crate::transforms;
use super::super::ir_gen::convert_yields_to_awaits;
use super::async_detection;
use super::generator_wrapper::generator_wrapper_target;
use super::PipelineContext;

impl PipelineContext {
    pub(super) fn run_transform_pipeline(
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

        // NOTE: do NOT run promote_const_bindings here. Env slots are shared
        // across closures; a binding that looks unreassigned in one body is often
        // mutated in a sibling. Promotion caused const-reassign / TDZ parse fails.

        // STAGE W16a2: while(true)+trailing break → do/while (after inlining cleans latch)
        {
            use rayon::prelude::*;
            let keys: Vec<u32> = all_ir.keys().copied().collect();
            let mut entries: Vec<(u32, Vec<Statement>)> = keys
                .into_iter()
                .filter_map(|id| all_ir.remove(&id).map(|s| (id, s)))
                .collect();
            entries.par_iter_mut().for_each(|(_, stmts)| {
                let old = std::mem::take(stmts);
                *stmts = transforms::convert_while_true_loops(old);
                let old = std::mem::take(stmts);
                *stmts = transforms::fold_guarded_loops(old);
            });
            for (id, stmts) in entries {
                all_ir.insert(id, stmts);
            }
        }

        // STAGE W16a3: second JSX pass after inlining (props often still variables before)
        {
            use rayon::prelude::*;
            let keys: Vec<u32> = all_ir.keys().copied().collect();
            let mut entries: Vec<(u32, Vec<Statement>)> = keys
                .into_iter()
                .filter_map(|id| all_ir.remove(&id).map(|s| (id, s)))
                .collect();
            entries.par_iter_mut().for_each(|(_, stmts)| {
                let old = std::mem::take(stmts);
                *stmts = transforms::reconstruct_jsx(old);
            });
            for (id, stmts) in entries {
                all_ir.insert(id, stmts);
            }
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
    pub(super) fn collapse_generator_wrappers(
        all_ir: &mut BTreeMap<u32, Vec<Statement>>,
        closure_ctx: &mut ClosureContext,
    ) {
        // A wrapper is any function whose body merely returns a generator object
        // created via CreateGenerator (`return (function*(){...})()` or bare
        // `return function*(){...}`, after env-slot init). The wrapper itself is
        // often a plain CreateClosure (not CreateGeneratorClosure); detecting by
        // shape, not by the is_generator flag on the wrapper, is required.
        // `generator_wrapper_target` only matches Function{is_generator:true}, so
        // the inner is a generator even if analysis missed marking it earlier.
        let mut replacements: Vec<(u32, u32)> = Vec::new();
        for (&fid, body) in all_ir.iter() {
            if let Some(inner) = generator_wrapper_target(body) {
                if inner != fid && all_ir.contains_key(&inner) {
                    replacements.push((fid, inner));
                }
            }
        }
        for (fid, inner) in replacements {
            if let Some(inner_body) = all_ir.get(&inner).cloned() {
                all_ir.insert(fid, inner_body);
                // Both ends are generators: wrapper becomes the callable function*,
                // inner was the CreateGenerator body (state machine / yields).
                closure_ctx.mark_generator(fid);
                closure_ctx.mark_generator(inner);
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

    pub(super) fn apply_post_ipa_transforms(all_ir: &mut BTreeMap<u32, Vec<Statement>>) {
        // Rename reserved JS keywords used as variable names (default → _default)
        for stmts in all_ir.values_mut() {
            transforms::rename_reserved_words(stmts);
        }

        // Fold incremental object/array construction into literals
        for stmts in all_ir.values_mut() {
            transforms::fold_slot_index_fills(stmts);
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

}
