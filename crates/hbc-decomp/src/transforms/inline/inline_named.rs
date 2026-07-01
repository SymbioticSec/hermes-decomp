// Post-pipeline variable inlining.
//
// After closure resolution and naming, variables are `Variable("tmp")`, `Variable("closure_0")`, etc.
// This pass inlines variables that are assigned once and used once, eliminating temporaries.

use crate::ir::{map_nested_bodies_mut, AssignTarget, Expression, Statement, Value};
use std::collections::BTreeMap;

use super::cleanup::cleanup_noise;
use super::counting::{
    apply_pending_to_stmt, count_var_defs_uses, flush_pending, substitute_vars_in_expr,
};

// Inline named variables (tmp*, closure_*, etc.) that are assigned once and used once,
// AND eliminate dead assignments (assigned but never read).
// Applied late in the pipeline after all naming passes.
pub fn inline_named_variables(stmts: Vec<Statement>) -> Vec<Statement> {
    // Phase 1: Count definitions and uses of all variables over the WHOLE
    // function (count_var_defs_uses recurses into nested blocks). The candidate
    // sets are derived once here and reused for nested blocks — recomputing them
    // per-block is unsound: a variable assigned inside an `if` branch but read
    // *after* the `if` has a block-local use count of 0, so it would be wrongly
    // treated as dead and its branch (then the whole `if`) eliminated.
    let mut def_count: BTreeMap<String, usize> = BTreeMap::new();
    let mut use_count: BTreeMap<String, usize> = BTreeMap::new();
    count_var_defs_uses(&stmts, &mut def_count, &mut use_count);

    let inline_candidates: std::collections::HashSet<String> = def_count
        .iter()
        .filter(|(name, &defs)| {
            if defs != 1 || !is_inlinable_name(name) {
                return false;
            }
            use_count.get(*name).copied().unwrap_or(0) == 1
        })
        .map(|(name, _)| name.clone())
        .collect();

    let multi_use_candidates: std::collections::HashSet<String> = def_count
        .iter()
        .filter(|(name, &defs)| {
            if defs != 1 || !is_inlinable_name(name) {
                return false;
            }
            let uses = use_count.get(*name).copied().unwrap_or(0);
            uses > 1 && uses <= 32
        })
        .map(|(name, _)| name.clone())
        .collect();

    let dead_candidates: std::collections::HashSet<String> = def_count
        .iter()
        .filter(|(name, &defs)| {
            defs >= 1
                && use_count.get(*name).copied().unwrap_or(0) == 0
                && is_dead_inlinable_name(name)
        })
        .map(|(name, _)| name.clone())
        .collect();

    inline_named_with_candidates(stmts, &inline_candidates, &multi_use_candidates, &dead_candidates)
}

// Process statements using PRE-COMPUTED (whole-function) candidate sets, and
// recurse into nested blocks with the SAME sets — see `inline_named_variables`.
fn inline_named_with_candidates(
    stmts: Vec<Statement>,
    inline_candidates: &std::collections::HashSet<String>,
    multi_use_candidates: &std::collections::HashSet<String>,
    dead_candidates: &std::collections::HashSet<String>,
) -> Vec<Statement> {
    if inline_candidates.is_empty() && dead_candidates.is_empty() && multi_use_candidates.is_empty() {
        return stmts;
    }

    // Phase 2: Process statements
    // For multi-use candidates, we collect their definitions first, then do a substitution pass.
    let mut multi_use_defs: BTreeMap<String, Expression> = BTreeMap::new();
    let mut result = Vec::new();
    let mut pending: BTreeMap<String, Expression> = BTreeMap::new();

    for stmt in stmts {
        // Extract variable name and value for both Assign and Let statements
        let var_info = match &stmt {
            Statement::Assign { target: AssignTarget::Variable(name), value } => {
                Some((name.clone(), value.clone(), false))
            }
            Statement::Let { name, value, .. } => {
                Some((name.clone(), value.clone(), true))
            }
            _ => None,
        };

        if let Some((name, value, _is_let)) = var_info {
                // Never inline/defer/eliminate function definitions -- they may be accessed via closure
                if matches!(value, Expression::Function { .. }) {
                    let mut stmt = stmt;
                    apply_pending_to_stmt(&mut stmt, &mut pending);
                    if !multi_use_defs.is_empty() {
                        apply_multi_use_to_stmt(&mut stmt, &multi_use_defs);
                    }
                    result.push(stmt);
                } else if inline_candidates.contains(&name) {
                    // Single-use inline candidate: defer until usage
                    let mut value = value;
                    substitute_vars_in_expr(&mut value, &pending);
                    substitute_vars_in_expr(&mut value, &multi_use_defs);

                    // Constants go to multi_use_defs so they work in nested blocks
                    if is_constant_expr(&value) {
                        multi_use_defs.insert(name.clone(), value);
                    } else {
                        if value.has_side_effects() && pending.values().any(|e| e.has_side_effects()) {
                            flush_pending(&mut pending, &mut result);
                        }
                        pending.insert(name.clone(), value);
                    }
                } else if multi_use_candidates.contains(&name) {
                    // Multi-use candidate: only inline if value is simple (no side effects, short)
                    let mut value = value;
                    substitute_vars_in_expr(&mut value, &pending);
                    substitute_vars_in_expr(&mut value, &multi_use_defs);
                    if is_simple_pure_expr(&value) {
                        multi_use_defs.insert(name.clone(), value);
                        // Don't emit the assignment -- it will be substituted at use sites
                    } else {
                        // Not simple enough: emit as normal
                        let mut stmt = Statement::Assign { target: AssignTarget::Variable(name.clone()), value };
                        apply_pending_to_stmt(&mut stmt, &mut pending);
                        result.push(stmt);
                    }
                } else if dead_candidates.contains(&name) {
                    // Dead assignment: keep only the side effect
                    // NEVER drop function definitions -- they may be accessed via closure slots
                    if matches!(value, Expression::Function { .. }) {
                        let mut stmt = stmt;
                        apply_pending_to_stmt(&mut stmt, &mut pending);
                        if !multi_use_defs.is_empty() {
                            apply_multi_use_to_stmt(&mut stmt, &multi_use_defs);
                        }
                        result.push(stmt);
                    } else if value.has_side_effects() {
                        let mut value = value;
                        substitute_vars_in_expr(&mut value, &pending);
                        substitute_vars_in_expr(&mut value, &multi_use_defs);
                        result.push(Statement::Expr(value));
                    }
                    // else: pure expression assigned to dead var -- drop entirely
                } else {
                    let mut stmt = stmt;
                    apply_pending_to_stmt(&mut stmt, &mut pending);
                    result.push(stmt);
                }
        } else {
            let mut stmt = stmt;
            apply_pending_to_stmt(&mut stmt, &mut pending);
            // Also substitute multi-use definitions
            if !multi_use_defs.is_empty() {
                apply_multi_use_to_stmt(&mut stmt, &multi_use_defs);
            }
            result.push(stmt);
        }
    }

    flush_pending(&mut pending, &mut result);

    // Phase 3: Recurse into sub-blocks to apply inlining there too, reusing the
    // whole-function candidate sets.
    for stmt in &mut result {
        recurse_inline_blocks(stmt, inline_candidates, multi_use_candidates, dead_candidates);
    }

    // Phase 4: Clean up noise
    result = cleanup_noise(result);

    result
}

// Recursively apply inlining to inner blocks of structured statements, EXCEPT
// loop bodies. Inlining inside a loop from isolated def/use counts is unsound: a
// loop-carried variable defined in the body but read in the condition or after
// the loop (via the back-edge) looks dead/single-use locally and would be wrongly
// eliminated (e.g. `sum = sum + i` dropped). Non-loop blocks (if/try/switch/block)
// are recursed with the SHARED whole-function candidate sets (passed in) so a
// variable used after the block is not treated as dead inside it.
fn recurse_inline_blocks(
    stmt: &mut Statement,
    inline_candidates: &std::collections::HashSet<String>,
    multi_use_candidates: &std::collections::HashSet<String>,
    dead_candidates: &std::collections::HashSet<String>,
) {
    match stmt {
        // Loop bodies: leave untouched (correctness over extra inlining).
        Statement::While { .. }
        | Statement::DoWhile { .. }
        | Statement::For { .. }
        | Statement::ForIn { .. }
        | Statement::ForOf { .. } => {}
        _ => map_nested_bodies_mut(stmt, |s| {
            inline_named_with_candidates(s, inline_candidates, multi_use_candidates, dead_candidates)
        }),
    }
}

// Check if an expression is a simple constant (integer, string, bool, null, undefined).
fn is_constant_expr(expr: &Expression) -> bool {
    matches!(expr, Expression::Value(Value::Constant(_)))
}

// Check if an expression is simple and pure (safe to duplicate for multi-use inlining).
fn is_simple_pure_expr(expr: &Expression) -> bool {
    match expr {
        // Simple values: variables, constants, parameters, globals
        Expression::Value(Value::Variable(_))
        | Expression::Value(Value::Parameter(_))
        | Expression::Value(Value::Constant(_))
        | Expression::Value(Value::Global)
        | Expression::Value(Value::NewTarget) => true,
        // Member access on a simple value: x.foo, x[0]
        Expression::Member { object, .. } => is_simple_pure_expr(object),
        // Unary on simple value: !x, typeof x, -x
        Expression::Unary { operand, .. } => is_simple_pure_expr(operand),
        // Short binary on simple values: a + b, a === b (but not calls within)
        Expression::Binary { left, right, .. } => is_simple_pure_expr(left) && is_simple_pure_expr(right),
        _ => false,
    }
}

fn apply_multi_use_to_stmt(stmt: &mut Statement, defs: &BTreeMap<String, Expression>) {
    match stmt {
        Statement::Assign { target, value } => {
            apply_multi_use_to_target(target, defs);
            substitute_vars_in_expr(value, defs);
        }
        Statement::Let { value, .. } => {
            substitute_vars_in_expr(value, defs);
        }
        Statement::Expr(e) => substitute_vars_in_expr(e, defs),
        Statement::Return(Some(e)) | Statement::Throw(e) => substitute_vars_in_expr(e, defs),
        // Loop/branch conditions: substitute (safe — these defs are constants /
        // simple pure values). Bodies are handled by the recursion, but loop
        // bodies are intentionally skipped, so doing the condition here ensures a
        // hoisted constant (e.g. a loop bound) still reaches `while (i < 5)`.
        Statement::While { condition, .. } | Statement::DoWhile { condition, .. } => {
            substitute_vars_in_expr(condition, defs);
        }
        Statement::If { condition, .. } => substitute_vars_in_expr(condition, defs),
        Statement::Switch { discriminant, .. } => substitute_vars_in_expr(discriminant, defs),
        _ => {}
    }
}

fn apply_multi_use_to_target(target: &mut AssignTarget, defs: &BTreeMap<String, Expression>) {
    match target {
        AssignTarget::Member { object, .. } => substitute_vars_in_expr(object, defs),
        AssignTarget::Index { object, key } => {
            substitute_vars_in_expr(object, defs);
            substitute_vars_in_expr(key, defs);
        }
        _ => {}
    }
}

// Shared list of generic register role prefixes (from register naming analysis).
// A name matching "prefix" or "prefixN" (e.g., "obj", "obj2") is generic.
const GENERIC_ROLE_PREFIXES: &[&str] = &[
    "num", "str", "obj", "fn", "arr", "bool", "length", "iter",
    "promise", "date", "err", "map", "set", "key", "val", "idx", "ref", "flag",
];

// Prefixes for intermediate wrapper variables generated by Babel/bundlers.
const WRAPPER_PREFIXES: &[&str] = &["_default", "_interop", "_extends"];

// Suffixes for intermediate computed values.
const INTERMEDIATE_SUFFIXES: &[&str] = &["Result", "Return", "Promise", "Callback", "Handler", "Wrapper"];

fn is_tmp_or_register(name: &str) -> bool {
    // tmp, tmp2, tmp3, ...
    if name == "tmp" || name.strip_prefix("tmp").is_some_and(|s| s.chars().all(|c| c.is_ascii_digit())) {
        return true;
    }
    // r0, r1, ...
    if name.strip_prefix('r').is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())) {
        return true;
    }
    false
}

fn is_generic_role_name(name: &str) -> bool {
    for prefix in GENERIC_ROLE_PREFIXES {
        if name == *prefix || name.strip_prefix(prefix).is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())) {
            return true;
        }
    }
    false
}

fn is_wrapper_or_intermediate(name: &str) -> bool {
    if WRAPPER_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return true;
    }
    for suffix in INTERMEDIATE_SUFFIXES {
        if name.ends_with(suffix) || name.contains(suffix) {
            return true;
        }
    }
    false
}

// More restrictive than `is_inlinable_name` -- excludes closure_* since they are
// accessed from other scopes via the closure environment.
pub fn is_dead_inlinable_name(name: &str) -> bool {
    is_tmp_or_register(name) || is_generic_role_name(name) || is_wrapper_or_intermediate(name)
    // Do NOT include closure_* -- they may be read from other function scopes
}

// Check if a variable name is a candidate for inlining (temporary/generic names only).
pub(super) fn is_inlinable_name(name: &str) -> bool {
    // NOTE: `closure_N` is deliberately excluded (as in `is_dead_inlinable_name`).
    // A resolved closure variable is shared with other function scopes; inlining
    // its value within one scope drops the binding the other scope still reads
    // (e.g. a captured counter `closure_0 += 1` mutated inside a returned closure).
    is_tmp_or_register(name) || is_generic_role_name(name) || is_wrapper_or_intermediate(name)
}
