// IR generation: transform bytecode into IR statements.
// Contains generate_ir() and build_closure_context_from_file().

use std::collections::BTreeMap;
use crate::analysis::{
    rename_registers, resolve_closures, ClosureContext,
    StructureAnalysis,
};
use crate::error::Result;
use crate::file::BytecodeFile;
use crate::ir::{BinaryOp, Expression, IRBuilder, IRBuilderOptions, Statement, Terminator};
use crate::opcode::BytecodeFormat;
use crate::transforms::{
    self, cleanup_statements, detect_class_patterns, detect_patterns, inline_expressions,
    optimize_statements, propagate, PropagationConfig,
};
use crate::util::is_valid_identifier;

use super::DecompileOptionsV2;

// Generate IR for a function (Analysis + Transform phases).
pub fn generate_ir(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    options: &DecompileOptionsV2,
    closure_ctx: Option<&ClosureContext>,
    perform_resolve: bool,
) -> Result<Vec<Statement>> {
    // STAGE F1: IR Build (bytecode -> CFG)
    let builder_options = IRBuilderOptions {
        resolve_strings: options.resolve_strings,
        include_offsets: options.include_offsets || options.assembly_mode,
        absolute_offsets: options.assembly_mode,
    };
    let mut builder = IRBuilder::new(file, format, builder_options);
    let mut cfg = builder.build_function(function_id)?;

    // STAGE F2: SSA / Live Range Splitting.
    // First resolve `globalThis` reads to the invariant value so the register
    // that held it is freed — the HBC >=97 allocator reuses it for unrelated
    // values, and an un-resolved read would force the SSA merge-freeze to
    // collapse the two live ranges under one name.
    transforms::resolve_global_reads(&mut cfg);
    transforms::transform_to_ssa(&mut cfg);

    // STAGE F3: Copy/Constant Propagation
    if options.propagate {
        propagate(&mut cfg, &PropagationConfig::default());
    }

    // STAGE F4: Expression Simplification
    if options.simplify {
        for block in cfg.blocks_mut() {
            crate::transforms::simplify_statements(&mut block.statements);
        }
    }

    // STAGE F5: Structure Recovery (CFG -> if/while/for/switch/try)
    let statements = if options.recover_structures {
        let analysis = StructureAnalysis::analyze(&cfg);
        analysis.root.to_statements(&cfg)
    } else {
        // Flatten blocks without structure recovery
        let mut stmts = Vec::new();
        for (id, block) in cfg.blocks_with_ids() {
            stmts.push(Statement::Comment(format!("{id}:")));
            stmts.extend(block.statements.clone());
            match &block.terminator {
                Terminator::Return(v) => stmts.push(Statement::Return(v.clone())),
                Terminator::Throw(e) => stmts.push(Statement::Throw(e.clone())),
                Terminator::Jump(t) => stmts.push(Statement::Goto(*t)),
                Terminator::Branch {
                    condition,
                    true_target,
                    false_target,
                } => {
                    stmts.push(Statement::CondGoto {
                        condition: condition.clone(),
                        target: *true_target,
                        fallthrough: *false_target,
                    });
                }
                Terminator::Switch {
                    value,
                    cases,
                    default,
                } => {
                    stmts.push(Statement::Comment("Switch dispatch".to_string()));
                    for (case_val, target) in cases {
                        let condition =
                            Expression::binary(BinaryOp::StrictEq, value.clone(), case_val.clone());
                        stmts.push(Statement::CondGoto {
                            condition,
                            target: *target,
                            fallthrough: *default,
                        });
                    }
                    stmts.push(Statement::Goto(*default));
                }
                _ => {}
            }
        }
        stmts
    };

    // Reconstruct for-of / for-in loops from the iterator protocol right after
    // structure recovery, BEFORE inlining folds the iterator registers away
    // (later detect_patterns can no longer see `iter = src[Symbol.iterator]()`).
    let statements = if options.recover_structures {
        // Modern protocol (HBC >= 74: IteratorBegin/IteratorNext/IteratorClose).
        let s = transforms::detect_for_of_loops(statements);
        // Legacy protocol (HBC 59-71: full spec {value,done} iterator).
        let s = transforms::detect_legacy_for_of(s);
        let s = transforms::detect_for_in_loops(s);
        // Array destructuring shares the iterator protocol; match it here, before
        // inlining folds the iterator registers away.
        transforms::detect_iterator_destructuring(s)
    } else {
        statements
    };

    // Check if this function contains generator patterns
    let has_generator = transforms::has_generator_patterns(&statements);
    // Determine if this is an async function from the closure context
    let is_async_function = closure_ctx
        .map(|ctx| ctx.is_async(function_id))
        .unwrap_or(false);

    // Apply high-level optimizations
    let mut statements = if options.simplify {
        // STAGE F6: Statement Optimization (if inversion, ternary, dead assign)
        let statements = optimize_statements(statements);
        // STAGE F7: Expression Inlining (single-use register elimination)
        let statements = inline_expressions(statements);

        // STAGE F8: Logic Transformation
        let mut statements = statements;
        transforms::transform_logic(&mut statements);
        let statements = statements;

        // STAGE F9: Concatenation Propagation
        let statements = transforms::data_flow::propagate_concatenation(statements);
        // STAGE F10: Pattern Detection (string concat, nullish, optional chaining, short-circuit)
        let statements = detect_patterns(statements);

        // STAGE F11: Class Pattern Detection (ES6 class reconstruction)
        let statements = detect_class_patterns(statements, file, format, options, closure_ctx);

        let mut statements = statements;

        // STAGE F12: Object/Array Literal Reconstruction
        transforms::transform_object_literals(&mut statements);
        transforms::arrays::transform_array_literals(&mut statements);
        // STAGE F13: Default Parameter Detection
        transforms::transform_default_params(&mut statements);

        // STAGE F14: Spread/Rest Operators
        transforms::transform_spread_rest(&mut statements);
        let statements = statements;

        // STAGE F15: Destructuring Detection
        let statements = transforms::detect_destructuring(statements);

        // STAGE F16: Generator/Async Pattern Detection
        let statements = if has_generator {
            let statements = transforms::detect_generator_patterns(statements, is_async_function);
            let statements = transforms::simplify_state_machine(statements);
            transforms::cleanup_generator_comments(statements)
        } else {
            statements
        };

        // STAGE F17: Yield-to-Await Conversion (async functions)
        let statements = if is_async_function {
            convert_yields_to_awaits(statements)
        } else {
            statements
        };

        // STAGE F18: Cleanup (basic + advanced)
        let statements = cleanup_statements(statements);
        let statements = transforms::cleanup_advanced(statements);

        // STAGE F19: Chain Access Optimization
        let statements = transforms::optimize_chain_access(statements);

        // STAGE F20: Ternary Return Optimization
        let statements = transforms::optimize_ternary_returns(statements);

        // STAGE F21: Logic Simplification (advanced)
        let statements = transforms::simplify_logic_advanced(statements);

        let mut statements = statements;

        // STAGE F22: CommonJS Export Inference + Name Inference
        let param_count = file
            .function_headers
            .get(function_id as usize)
            .map(|h| h.param_count())
            .unwrap_or(0);

        if let Some(names) = transforms::exports::infer_commonjs_names(&mut statements, param_count)
        {
            transforms::exports::rename_param_registers(&mut statements, &names);
        }

        transforms::infer_names(&mut statements);
        let statements = statements;

        // STAGE F23: Register Naming (analyze + debug info merge + rename)
        let statements = super::apply_register_naming(statements, file, function_id);

        // STAGE F24: Semantic Variable Naming
        let mut statements = transforms::infer_variable_names(statements);

        // STAGE F25: Final Simplification
        crate::transforms::simplify_statements(&mut statements);
        statements
    } else {
        statements
    };

    // STAGE F26: Closure Resolution (if context provided)
    if perform_resolve {
        if let Some(ctx) = closure_ctx {
            let closure_info = ctx.get_closure_info_for(function_id);
            if !closure_info.slots.is_empty() {
                statements = resolve_closures(statements, &closure_info);
            }
        }
    }

    Ok(statements)
}

// Build a closure context by analyzing all functions.
// This enables cross-function closure resolution.
pub fn build_closure_context_from_file(
    file: &BytecodeFile,
    format: &BytecodeFormat,
) -> Result<ClosureContext> {
    use rayon::prelude::*;

    let builder_options = IRBuilderOptions {
        resolve_strings: true,
        include_offsets: false,
        absolute_offsets: false,
    };

    // Parallel: compute per-function data (name + analyzed statements).
    // rayon preserves input order in the output Vec.
    type FunctionIr = (u32, Option<String>, Vec<Statement>);
    let results: Vec<Option<FunctionIr>> = (0..file.function_headers.len())
        .into_par_iter()
        .map(|i| {
            let function_id = i as u32;
            let header = &file.function_headers[i];

            let func_name = file
                .string_at(header.function_name())
                .filter(|e| !e.value.is_empty() && is_valid_identifier(&e.value))
                .map(|e| e.value.clone());

            let mut builder = IRBuilder::new(file, format, builder_options.clone());
            let Ok(mut cfg) = builder.build_function(function_id) else {
                return None;
            };
            propagate(&mut cfg, &PropagationConfig::default());

            let analysis = StructureAnalysis::analyze(&cfg);
            let statements = analysis.root.to_statements(&cfg);

            let debug_names = if let Some(debug_info) = &file.debug_info {
                let scope_offset = debug_info
                    .source_locations
                    .get(&function_id)
                    .and_then(|locs| locs.iter().find_map(|l| l.scope_offset));
                debug_info.build_variable_map(scope_offset)
            } else {
                BTreeMap::new()
            };

            let statements = if !debug_names.is_empty() {
                rename_registers(statements, &debug_names)
            } else {
                statements
            };

            Some((function_id, func_name, statements))
        })
        .collect();

    // Sequential: build ClosureContext from computed results (order is preserved).
    let mut ctx = ClosureContext::new();
    for item in results.into_iter().flatten() {
        let (function_id, func_name, statements) = item;
        if let Some(name) = func_name {
            ctx.add_function_name(function_id, name);
        }
        ctx.analyze_function(function_id, &statements);
    }

    // Propagate async flag from outer wrappers to inner generators
    ctx.propagate_async_to_generators();

    Ok(ctx)
}

// Convert all `Expression::Yield` to `Expression::Await` in a statement list.
// Used for async functions where the CFG-level generator transform emitted Yield
// but the function is actually async.
pub(crate) fn convert_yields_to_awaits(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts.into_iter().map(convert_yield_stmt).collect()
}

fn convert_yield_stmt(stmt: Statement) -> Statement {
    match stmt {
        Statement::Assign { target, value } => Statement::Assign {
            target,
            value: convert_yield_expr(value),
        },
        Statement::Expr(e) => Statement::Expr(convert_yield_expr(e)),
        Statement::Return(Some(e)) => Statement::Return(Some(convert_yield_expr(e))),
        Statement::Throw(e) => Statement::Throw(convert_yield_expr(e)),
        Statement::Let { name, value, kind } => Statement::Let {
            name,
            value: convert_yield_expr(value),
            kind,
        },
        Statement::If { condition, then_body, else_body } => Statement::If {
            condition: convert_yield_expr(condition),
            then_body: convert_yields_to_awaits(then_body),
            else_body: convert_yields_to_awaits(else_body),
        },
        Statement::While { condition, body } => Statement::While {
            condition: convert_yield_expr(condition),
            body: convert_yields_to_awaits(body),
        },
        Statement::For { init, condition, update, body } => Statement::For {
            init: init.map(|s| Box::new(convert_yield_stmt(*s))),
            condition: condition.map(convert_yield_expr),
            update: update.map(|s| Box::new(convert_yield_stmt(*s))),
            body: convert_yields_to_awaits(body),
        },
        Statement::TryCatch { try_body, catch_param, catch_body, finally_body } => Statement::TryCatch {
            try_body: convert_yields_to_awaits(try_body),
            catch_param,
            catch_body: convert_yields_to_awaits(catch_body),
            finally_body: convert_yields_to_awaits(finally_body),
        },
        Statement::Block(inner) => Statement::Block(convert_yields_to_awaits(inner)),
        other => other,
    }
}

fn convert_yield_expr(expr: Expression) -> Expression {
    match expr {
        Expression::Yield { value, .. } => Expression::Await(Box::new(convert_yield_expr(*value))),
        Expression::Call { callee, arguments } => Expression::Call {
            callee: Box::new(convert_yield_expr(*callee)),
            arguments: arguments.into_iter().map(convert_yield_expr).collect(),
        },
        Expression::Binary { op, left, right } => Expression::Binary {
            op,
            left: Box::new(convert_yield_expr(*left)),
            right: Box::new(convert_yield_expr(*right)),
        },
        Expression::Unary { op, operand } => Expression::Unary {
            op,
            operand: Box::new(convert_yield_expr(*operand)),
        },
        Expression::Conditional { condition, then_expr, else_expr } => Expression::Conditional {
            condition: Box::new(convert_yield_expr(*condition)),
            then_expr: Box::new(convert_yield_expr(*then_expr)),
            else_expr: Box::new(convert_yield_expr(*else_expr)),
        },
        Expression::Member { object, property, optional } => Expression::Member {
            object: Box::new(convert_yield_expr(*object)),
            property,
            optional,
        },
        Expression::Assignment { target, value } => Expression::Assignment {
            target: Box::new(convert_yield_expr(*target)),
            value: Box::new(convert_yield_expr(*value)),
        },
        other => other,
    }
}
