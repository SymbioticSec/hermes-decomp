use crate::ir::{AssignTarget, Expression, Statement, Value, VarKind};
use std::collections::{BTreeMap, HashSet};

mod patterns;
mod scan;
mod writes;

use patterns::collect_pattern_names;
use scan::{collect_scope_info, free_captured_vars};
use writes::count_writes;

#[cfg(test)]
mod tests;

// Insert `const`/`let` declarations for first-assignments of variables.
// Converts `x = expr;` into `const x = expr;` (if never reassigned) or `let x = expr;`.
// Skips parameters and variables already declared via `Statement::Let`.
pub fn insert_declarations(stmts: &mut Vec<Statement>, params: &[String]) {
    insert_declarations_with_extra_writes(stmts, params, &BTreeMap::new());
}

// Like `insert_declarations`, but folds in write counts from nested/descendant
// functions (closures, getters) that mutate free variables declared here.
// Without this, a parent emits `const tmp = …` while an inlined child does
// `tmp = …` → "Assignment to constant variable".
pub fn insert_declarations_with_extra_writes(
    stmts: &mut Vec<Statement>,
    params: &[String],
    extra_writes: &BTreeMap<String, usize>,
) {
    insert_declarations_with_outer(stmts, params, extra_writes, &HashSet::new(), false);
}

// Like `insert_declarations_with_extra_writes`, but:
// - `outer_names` are ancestor *env-slot* captures and must stay Assign
//   (not every ancestor local — that dropped `let obj` / `let _Error` in children)
// - `skip_env_slots` leaves `closure_*` / `cN` as Assign (nested handlers)
pub fn insert_declarations_with_outer(
    stmts: &mut Vec<Statement>,
    params: &[String],
    extra_writes: &BTreeMap<String, usize>,
    outer_names: &HashSet<String>,
    skip_env_slots: bool,
) {
    // Phase 1: Count total writes per variable across the entire function body
    let mut write_count: BTreeMap<String, usize> = BTreeMap::new();
    let mut let_declared: std::collections::HashSet<String> = std::collections::HashSet::new();
    count_writes(stmts, &mut write_count, &mut let_declared);
    for (name, n) in extra_writes {
        *write_count.entry(name.clone()).or_insert(0) += *n;
    }

    // A variable whose first occurrence is a READ is free / captured from an
    // enclosing scope (e.g. counter `c0` mutated inside a returned closure, or
    // legacy `closure_N`). Re-declaring it here shadows the outer binding and
    // causes TDZ (`const c0 = c0 + 1`). Skip declaring free vars; the owner
    // scope still declares them (as `let` when mutated across scopes).
    let free_closures = free_captured_vars(stmts);

    let param_set: std::collections::HashSet<&str> = params.iter().map(|s| s.as_str()).collect();
    let mut declared: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Phase 1b: Hoist loop-carried variables. A variable assigned ONLY inside
    // loop(s) but read OUTSIDE the loop would otherwise get its `let` placed
    // inside the loop body, leaving the outside use referencing an out-of-scope
    // binding. Declare those at the function top (`let x;`) and keep their
    // in-loop assignments as plain assignments.
    let mut assigned_in_loop = std::collections::HashSet::new();
    let mut assigned_out_loop = std::collections::HashSet::new();
    let mut ref_out_loop = std::collections::HashSet::new();
    collect_scope_info(stmts, false, &mut assigned_in_loop, &mut assigned_out_loop, &mut ref_out_loop);

    let skip_decl = |v: &str| -> bool {
        param_set.contains(v)
            || let_declared.contains(v)
            || outer_names.contains(v)
            || (skip_env_slots && is_env_slot_name(v))
    };
    let mut hoist: Vec<String> = assigned_in_loop
        .iter()
        .filter(|v| {
            !assigned_out_loop.contains(*v)
                && ref_out_loop.contains(*v)
                && !skip_decl(v)
                && is_valid_js_identifier(v)
        })
        .cloned()
        .collect();
    // Hoist destructuring-pattern names: patterns are emitted as bare assignments
    // (`[a,b] = e`, `({x,y} = e)`), so their names must be declared once at the
    // top, two patterns can share a register-derived name (`x`/`y` reused), and
    // an inline `let` per pattern would redeclare.
    let mut pattern_names: Vec<String> = Vec::new();
    collect_pattern_names(stmts, &mut pattern_names);
    for name in pattern_names {
        if !skip_decl(&name) && is_valid_js_identifier(&name) {
            hoist.push(name);
        }
    }
    hoist.sort();
    hoist.dedup();

    if !hoist.is_empty() {
        let mut decls: Vec<Statement> = hoist
            .iter()
            .map(|name| Statement::Let {
                name: name.clone(),
                value: Expression::constant(crate::ir::Constant::Undefined),
                kind: VarKind::Let,
            })
            .collect();
        for name in &hoist {
            declared.insert(name.clone());
        }
        decls.append(stmts);
        *stmts = decls;
    }

    // Phase 2: Walk statements, converting first assignment to declaration
    insert_decls_in_block(
        stmts,
        &write_count,
        &let_declared,
        &param_set,
        &free_closures,
        &mut DeclSkip {
            outer_names,
            skip_env_slots,
            declared: &mut declared,
        },
    );
}

struct DeclSkip<'a> {
    outer_names: &'a HashSet<String>,
    skip_env_slots: bool,
    declared: &'a mut HashSet<String>,
}

fn is_env_slot_name(name: &str) -> bool {
    // closure_0, closure_1_2, c0, c12 — Hermes env / counter bindings
    if let Some(rest) = name.strip_prefix("closure_") {
        return !rest.is_empty()
            && rest.chars().all(|c| c.is_ascii_digit() || c == '_')
            && rest.chars().any(|c| c.is_ascii_digit());
    }
    name.len() >= 2
        && name.starts_with('c')
        && name[1..].chars().all(|c| c.is_ascii_digit())
}

fn insert_decls_in_block(
    stmts: &mut [Statement],
    write_count: &BTreeMap<String, usize>,
    let_declared: &HashSet<String>,
    params: &HashSet<&str>,
    free_closures: &HashSet<String>,
    skip: &mut DeclSkip<'_>,
) {
    for stmt in stmts.iter_mut() {
        match stmt {
            Statement::Assign { target: AssignTarget::Variable(name), value } => {
                // Skip invalid JS identifiers (numbers, strings, reserved words used as names)
                if is_valid_js_identifier(name)
                    && !params.contains(name.as_str())
                    && !let_declared.contains(name)
                    && !skip.declared.contains(name)
                    && !free_closures.contains(name)
                    && !skip.outer_names.contains(name)
                    && !(skip.skip_env_slots && is_env_slot_name(name))
                    && !is_self_assignment_var(name, value)
                {
                    skip.declared.insert(name.clone());
                    let writes = write_count.get(name).copied().unwrap_or(1);
                    // Prefer `let` when mutated more than once, or when the name
                    // looks like a Hermes env slot (`c0`, `closure_N`), those
                    // are often mutated from nested closures even if this
                    // function only shows one write.
                    let kind = if writes > 1 || is_env_slot_name(name) {
                        VarKind::Let
                    } else {
                        VarKind::Const
                    };
                    *stmt = Statement::Let {
                        name: name.clone(),
                        value: value.clone(),
                        kind,
                    };
                }
            }
            Statement::Assign { target: AssignTarget::Register(r), value } => {
                let name = format!("r{r}");
                if !params.contains(name.as_str())
                    && !let_declared.contains(&name)
                    && !skip.declared.contains(&name)
                {
                    skip.declared.insert(name.clone());
                    let writes = write_count.get(&name).copied().unwrap_or(1);
                    let kind = if writes <= 1 { VarKind::Const } else { VarKind::Let };
                    *stmt = Statement::Let {
                        name,
                        value: value.clone(),
                        kind,
                    };
                }
            }
            Statement::If { then_body, else_body, .. } => {
                insert_decls_in_block(then_body, write_count, let_declared, params, free_closures, skip);
                insert_decls_in_block(else_body, write_count, let_declared, params, free_closures, skip);
            }
            Statement::While { body, .. } | Statement::DoWhile { body, .. }
            | Statement::For { body, .. } | Statement::ForIn { body, .. }
            | Statement::ForOf { body, .. } => {
                insert_decls_in_block(body, write_count, let_declared, params, free_closures, skip);
            }
            Statement::Block(inner) => {
                insert_decls_in_block(inner, write_count, let_declared, params, free_closures, skip);
            }
            Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
                insert_decls_in_block(try_body, write_count, let_declared, params, free_closures, skip);
                insert_decls_in_block(catch_body, write_count, let_declared, params, free_closures, skip);
                insert_decls_in_block(finally_body, write_count, let_declared, params, free_closures, skip);
            }
            Statement::Switch { cases, default, .. } => {
                for (_, body) in cases.iter_mut() {
                    insert_decls_in_block(body, write_count, let_declared, params, free_closures, skip);
                }
                if let Some(d) = default {
                    insert_decls_in_block(d, write_count, let_declared, params, free_closures, skip);
                }
            }
            _ => {}
        }
    }
}

// Check if a name is a valid JavaScript identifier (not a number, not a string literal).
fn is_valid_js_identifier(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    // Must not start with a digit
    // SAFETY: name.is_empty() is checked above
    let first = match name.chars().next() {
        Some(c) => c,
        None => return false,
    };
    if first.is_ascii_digit() || first == '"' || first == '\'' {
        return false;
    }
    // Must be alphanumeric + _ + $
    name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

// Check if assignment is a self-assignment: `name = name`
fn is_self_assignment_var(name: &str, value: &Expression) -> bool {
    matches!(value, Expression::Value(Value::Variable(v)) if v == name)
}

// Aggregate writes from nested/descendant function bodies so the parent scope
// chooses `let` when a child mutates a shared name.
//
// Two cases matter:
// 1. Free (read-before-write) captures, classic closure mutation
//    (`const c = 0; () => { c = c + 1 }` → parent must be `let`).
// 2. Write-first reassignment of an outer name, constructors/getters often do
//    `items = […]` without reading first. The free-only path misses these; we
//    still count any nested `Assign` to a Variable that is not an explicit
//    `Let` in that body. Extra keys that never appear in the parent are ignored
//    by `insert_declarations` (harmless). Same-name shadowing may demote a
//    parent `const` → `let`, which stays valid JS.
pub fn extra_writes_from_nested_bodies(nested_bodies: &[&[Statement]]) -> BTreeMap<String, usize> {
    let mut extra: BTreeMap<String, usize> = BTreeMap::new();
    for body in nested_bodies {
        let free = free_captured_vars(body);
        let mut writes: BTreeMap<String, usize> = BTreeMap::new();
        let mut lets = std::collections::HashSet::new();
        count_writes(body, &mut writes, &mut lets);

        for (name, c) in writes {
            // Explicit `let/const name = …` in the child owns the binding; those
            // writes stay local and must not force the parent to `let`.
            if lets.contains(&name) {
                continue;
            }
            if free.contains(&name) {
                // Free mutation: at least 2 so a single nested write forces let.
                *extra.entry(name).or_insert(0) += c.max(2);
            } else {
                // Write-first (or mixed) assign without a local Let, may target
                // an outer binding once inlined/rendered. +c is enough for
                // parent_local(1) + nested(1) → let.
                *extra.entry(name).or_insert(0) += c.max(1);
            }
        }
    }
    extra
}
