// PipelineContext: pre-computed analysis context for efficient code generation.
// Built once (expensive), then used to generate code for individual functions cheaply.

mod async_detection;
mod codegen;
mod closures;
mod ir_build;
mod naming;
mod rendering;
mod generator_wrapper;
mod transforms_phase;

use std::collections::BTreeMap;
use std::sync::Arc;
use crate::analysis::ClosureContext;
use crate::error::Result;
use crate::file::BytecodeFile;
use crate::ir::Statement;
use crate::opcode::BytecodeFormat;
use crate::transforms;

use super::{
    build_closure_context_from_file, get_function_params, DecompileOptionsV2,
};

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
    // parent function id → its direct child function ids, inverted once from the
    // closure context's child → parent map. `extra_writes_for_function` walks this
    // per function, so building it per call was quadratic over the whole bundle.
    pub(super) child_functions: BTreeMap<u32, Vec<u32>>,
    // function id → env-slot names owned by its ancestors, precomputed once
    // (top-down, O(n)). Read by both the bulk render path and the single-function
    // path so neither rebuilds the whole map per function.
    pub(super) ancestor_env_slots: BTreeMap<u32, std::collections::HashSet<String>>,
    // Recovered Reanimated worklet sources (function name → original source),
    // extracted from `__initData.code` string constants in the bundle.
    pub(super) worklet_sources: BTreeMap<String, String>,
}

impl PipelineContext {
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

        let n_funcs = file.header.function_count;
        super::progress::status(format!(
            "pipeline: {} functions (HBC v{})",
            n_funcs, file.header.version
        ));

        // STAGE W1: Closure Context Build
        let phase = super::progress::Phase::start("closure context");
        let t = std::time::Instant::now();
        let mut closure_ctx = Some(build_closure_context_from_file(file, format)?);
        log::debug!("[pipeline] closure context: {:.2?}", t.elapsed());
        phase.finish();

        // STAGE W2: Metro Detection
        let phase = super::progress::Phase::start("Metro module detection");
        let mut registry = Self::build_metro_registry(file, format);
        let n_modules = registry.modules.len();
        phase.finish_with(format!("{n_modules} modules"));

        // STAGE W3-W4: Generate optimized IR (parallel) + closure analysis
        let phase = super::progress::Phase::start(format!("IR generation ({n_funcs} functions)"));
        let mut all_ir = Self::generate_all_optimized_ir(file, format, &options, &mut closure_ctx);
        phase.finish();

        // STAGE W5-W11: Name resolution (module names, closures, exports, IPA)
        let phase = super::progress::Phase::start("naming / IPA / closures");
        let mut global_analysis = Self::run_naming_pipeline(
            &mut all_ir, &mut registry, &mut closure_ctx, file,
        );
        phase.finish();

        // STAGE W12-W16: Transform pipeline (inlining, async detection, post-IPA)
        let phase = super::progress::Phase::start("transforms (inline / async / cleanup)");
        Self::run_transform_pipeline(
            &mut all_ir, &mut closure_ctx, &mut global_analysis, file,
        );
        phase.finish();

        // Recover original worklet sources from embedded `__initData.code` strings.
        let worklet_sources = transforms::collect_worklet_sources(&all_ir);
        log::debug!("[pipeline] recovered {} worklet sources", worklet_sources.len());

        // Invert the closure child → parent map once (parent → children), so the
        // per-function extra-writes walk does not rebuild it 100k+ times.
        let mut child_functions: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        if let Some(cctx) = closure_ctx.as_ref() {
            for (&child, &parent) in &cctx.parent_function {
                child_functions.entry(parent).or_default().push(child);
            }
        }

        // STAGE W16e: Hoist eager inline module loads (importDefault(N)/require(N)
        // repeated at every use site) into one module-level binding, which the ESM
        // classifier then lifts into an import. Runs before inline body rendering so
        // the rewritten descendant bodies are the ones rendered.
        {
            let phase = super::progress::Phase::start("import hoisting");
            let t = std::time::Instant::now();
            let mut hoist_params: BTreeMap<u32, Vec<String>> = BTreeMap::new();
            for &fid in all_ir.keys() {
                let names = if let Some(pn) = global_analysis.param_names.get(&fid) {
                    pn.iter()
                        .enumerate()
                        .map(|(i, n)| n.clone().unwrap_or_else(|| format!("arg{i}")))
                        .collect()
                } else {
                    get_function_params(file, fid)
                };
                hoist_params.insert(fid, names);
            }
            transforms::hoist_module_loaders(
                &mut all_ir,
                &registry,
                &child_functions,
                &hoist_params,
            );
            log::debug!("[pipeline] import hoisting: {:.2?}", t.elapsed());
            phase.finish();
        }

        // STAGE W17: Inline body rendering
        let mut ctx = PipelineContext {
            all_ir,
            registry,
            closure_ctx,
            global_analysis,
            inline_bodies: Arc::new(BTreeMap::new()),
            child_functions,
            ancestor_env_slots: BTreeMap::new(),
            worklet_sources,
        };
        // Precompute the ancestor env-slot names once (top-down, O(n)); both the
        // bulk render path and the per-function path read this instead of rebuilding.
        ctx.ancestor_env_slots = ctx.precompute_ancestor_env_slot_names();

        let phase = super::progress::Phase::start("inline body rendering");
        let t = std::time::Instant::now();
        ctx.build_all_inline_bodies(file);
        log::debug!("[pipeline] inline body rendering: {:.2?} ({} of {} functions)", t.elapsed(), ctx.inline_bodies.len(), file.header.function_count);
        phase.finish_with(format!("{} bodies", ctx.inline_bodies.len()));

        log::debug!("[pipeline] exception handlers: {} functions with try/catch", file.exception_handlers.len());
        log::debug!("[pipeline] TOTAL: {:.2?}", total_start.elapsed());
        super::progress::status(format!(
            "analysis complete in {:.1}s",
            total_start.elapsed().as_secs_f64()
        ));

        Ok(ctx)
    }
}
