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

        // STAGE W16f: object-key names (`{ login: closure_1_0 }`) are often only
        // visible after slot-fill folding and v98 generator reconstruct. Re-run
        // closure naming so a captured param stored under a property key is named
        // from that key (ground truth), including the parent parameter.
        if let Some(ctx) = closure_ctx.as_mut() {
            let renamed = transforms::rename_closure_variables_cross_function(
                all_ir,
                ctx,
                &mut global_analysis.param_names,
            );
            let inherited = transforms::inherit_ancestor_closure_names(all_ir, ctx);
            if renamed > 0 || inherited > 0 {
                log::debug!(
                    "[pipeline] post-reconstruct object-key naming: {renamed} closures, {inherited} inherited"
                );
            }
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
        // The wrapper collapse above just marked new generators, and W14 marked the
        // wrappers async. Propagate now so a generator whose parent is async is
        // known to be async BEFORE the reconstruction loop below, which is what
        // decides whether its `yield`s become `await`s.
        closure_ctx.propagate_async_to_generators();

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
                let Some(lifted) = transforms::try_reconstruct_generator_v98(&body) else {
                    // The machine did not lift, so it stays exactly as decoded.
                    // The cleanup below reads data flow to decide what is dead,
                    // and in a raw resume machine the flow runs through the label
                    // and status slots, which those passes cannot follow: they
                    // then delete live code. That is how the header build in
                    // `piloteAuthHeaders`, `Bearer ` and `x-refresh-token`
                    // included, disappeared from the output while still being
                    // present in the per function decompile of the same body.
                    all_ir.insert(fid, body);
                    continue;
                };
                let mut body = lifted;
                // Reconstruct runs after the W14 yield→await pass, so a v98
                // machine that just became `yield` still needs the async rewrite.
                if closure_ctx.is_async(fid) {
                    body = convert_yields_to_awaits(body);
                }
                // Flat body: inline `obj2 = {login}; obj1.body = obj2` then
                // fold placeholder members into the literal, then drop the
                // leftover state-machine temps (`c1 = tmp3`, `dependencyMap = 0`).
                body = transforms::inline_named_variables(body);
                transforms::fold_slot_index_fills(&mut body);
                body = transforms::inline_named_variables(body);
                transforms::fold_slot_index_fills(&mut body);
                body = transforms::remove_dead_temp_bindings(body);
                body = transforms::eliminate_dead_stores(body);
                body = drop_unread_bookkeeping(body);
                body = drop_duplicate_bare_requires(body);
                all_ir.insert(fid, body);
            }
        }

        // W14 ran before reconstruct, so v98 machines had no `yield` yet and
        // detect may have missed `asyncGeneratorStep(undefined, function*(){})`.
        // Re-scan and convert now that bodies are flat.
        let async_ids = async_detection::detect_async_generator_wrappers(all_ir);
        for func_id in &async_ids {
            closure_ctx.mark_async(*func_id);
            if let Some(body) = all_ir.remove(func_id) {
                all_ir.insert(*func_id, convert_yields_to_awaits(body));
            }
        }
        async_detection::strip_redundant_async_helpers(all_ir, closure_ctx);

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

// Drop leftover state-machine bookkeeping that reconstruct no longer reads
// (`c1 = tmp3`, `dependencyMap = 0`). Only unread trivial copies/scalars.
fn drop_unread_bookkeeping(stmts: Vec<crate::ir::Statement>) -> Vec<crate::ir::Statement> {
    use crate::ir::{AssignTarget, Expression, Statement, Value, Visitor};
    use std::collections::HashMap;

    struct Reads<'a>(&'a mut HashMap<String, u32>);
    impl Visitor<'_> for Reads<'_> {
        fn visit_expression(&mut self, e: &Expression) {
            if let Expression::Value(Value::Variable(n)) = e {
                *self.0.entry(n.clone()).or_insert(0) += 1;
            }
            self.walk_expression(e);
        }
    }
    let mut reads = HashMap::new();
    {
        let mut c = Reads(&mut reads);
        for s in &stmts {
            c.visit_statement(s);
        }
    }
    stmts
        .into_iter()
        .filter(|stmt| match stmt {
            Statement::Let { name, value, .. } | Statement::Assign {
                target: AssignTarget::Variable(name),
                value,
            } => {
                if !is_bookkeeping_name(name) {
                    return true;
                }
                if reads.get(name).copied().unwrap_or(0) != 0 {
                    return true;
                }
                !is_trivial_bookkeeping_value(value)
            }
            _ => true,
        })
        .collect()
}

fn is_bookkeeping_name(name: &str) -> bool {
    if name == "dependencyMap" || name == "_dependencyMap" {
        return true;
    }
    if name == "tmp" || name.strip_prefix("tmp").is_some_and(|r| r.chars().all(|c| c.is_ascii_digit())) {
        return true;
    }
    let mut ch = name.chars();
    matches!(
        (ch.next(), ch.as_str()),
        (Some('c' | 'v'), rest) if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
    )
}

fn drop_duplicate_bare_requires(stmts: Vec<crate::ir::Statement>) -> Vec<crate::ir::Statement> {
    use crate::ir::{Expression, Statement};
    fn require_key(e: &Expression) -> Option<String> {
        let Expression::Call { arguments, .. } = e else { return None };
        let args = if arguments.len() >= 2
            && matches!(
                &arguments[0],
                Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::Undefined))
            ) {
            &arguments[1..]
        } else {
            arguments.as_slice()
        };
        match args.first() {
            Some(Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::Integer(id)))) => {
                Some(format!("i{id}"))
            }
            Some(Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::String(s)))) => {
                Some(format!("s{s}"))
            }
            _ => None,
        }
    }
    use crate::ir::Visitor;
    struct Keys<'a>(&'a mut std::collections::HashSet<String>, bool);
    impl Visitor<'_> for Keys<'_> {
        fn visit_expression(&mut self, e: &Expression) {
            if let Some(k) = require_key(e) {
                if self.1 {
                    self.0.insert(k);
                }
            }
            self.walk_expression(e);
        }
    }
    let mut used = std::collections::HashSet::new();
    {
        let mut c = Keys(&mut used, true);
        for s in &stmts {
            if !matches!(s, Statement::Expr(e) if require_key(e).is_some()) {
                c.visit_statement(s);
            }
        }
    }
    stmts
        .into_iter()
        .filter(|s| match s {
            Statement::Expr(e) => require_key(e).is_none_or(|k| !used.contains(&k)),
            _ => true,
        })
        .collect()
}

fn is_trivial_bookkeeping_value(e: &crate::ir::Expression) -> bool {
    use crate::ir::{Constant, Expression, Value};
    matches!(
        e,
        Expression::Value(Value::Variable(_))
            | Expression::Value(Value::Register(_))
            | Expression::Value(Value::Parameter(_))
            | Expression::Value(Value::Constant(
                Constant::Integer(_) | Constant::Null | Constant::Undefined | Constant::Bool(_)
            ))
    )
}
