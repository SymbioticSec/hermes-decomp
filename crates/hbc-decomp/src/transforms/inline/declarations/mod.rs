use crate::ir::{AssignTarget, Binding, Expression, Statement, Value, VarKind};
use std::collections::{BTreeMap, HashSet};

mod patterns;
mod scan;
mod writes;

use patterns::collect_pattern_names;
use scan::{
    collect_scope_info, free_captured_vars, names_assigned_in_nested_bodies_and_used_outside,
};
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

// `own_slots`: the names of this function's own environment slots, from the
// closure context. A captured variable the function never initialises
// (`var pending;` written and read by its closures only) has a slot and a
// name but no statement here, so nothing declared it and every closure read
// an unbound name. It is declared `let name;` at the top.
pub fn insert_declarations_with_slots(
    stmts: &mut Vec<Statement>,
    params: &[String],
    extra_writes: &BTreeMap<String, usize>,
    outer_names: &HashSet<String>,
    own_slots: &HashSet<String>,
) {
    insert_declarations_impl(stmts, params, extra_writes, outer_names, own_slots);
}

// Like `insert_declarations_with_extra_writes`, but:
// - `outer_names` are ancestor *env-slot* captures and must stay Assign
//   (not every ancestor local — that dropped `let obj` / `let _Error` in children)
// - `skip_env_slots` is retained for callers. A `closure_*` / `cN` written in
//   this function is declared here unless its name is already in `outer_names`.
//   Skipping every env slot left tens of thousands of bare assignments.
pub fn insert_declarations_with_outer(
    stmts: &mut Vec<Statement>,
    params: &[String],
    extra_writes: &BTreeMap<String, usize>,
    outer_names: &HashSet<String>,
    skip_env_slots: bool,
) {
    let _ = skip_env_slots;
    insert_declarations_impl(stmts, params, extra_writes, outer_names, &HashSet::new());
}

fn insert_declarations_impl(
    stmts: &mut Vec<Statement>,
    params: &[String],
    extra_writes: &BTreeMap<String, usize>,
    outer_names: &HashSet<String>,
    own_slots: &HashSet<String>,
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
    collect_scope_info(
        stmts,
        false,
        &mut assigned_in_loop,
        &mut assigned_out_loop,
        &mut ref_out_loop,
    );

    // A class or function declaration binds its name for the whole body; a
    // hoisted `let` of the same name would bind it twice (`let t;` above
    // `class t`).
    let declared_by_decl = declaration_names(stmts);
    let skip_decl = |v: &str| -> bool {
        param_set.contains(v)
            || let_declared.contains(v)
            || outer_names.contains(v)
            || declared_by_decl.contains(v)
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
    // The same escape happens out of a branch, a try or a switch case: `error`
    // set in both arms of an `if` and thrown after it was declared in the first
    // arm and unbound after the `if`.
    for name in names_assigned_in_nested_bodies_and_used_outside(stmts) {
        if !skip_decl(&name) && !free_closures.contains(&name) && is_valid_js_identifier(&name) {
            hoist.push(name);
        }
    }
    // A name read before it is written, yet written in this function and
    // bound by no enclosing scope, is a local the loop carries (`result`,
    // `computed` in lodash's baseExtremum: read at the top of the body,
    // stored at its end). It was taken for a capture and never declared.
    for name in &free_closures {
        let written = write_count.get(name).copied().unwrap_or(0) > 0;
        if written
            && !skip_decl(name)
            && !is_env_slot_name(name)
            && !name.starts_with("outer")
            && is_valid_js_identifier(name)
        {
            hoist.push(name.clone());
        }
    }
    // An own slot no statement of this body binds: a captured variable
    // initialised only by the closures that share it.
    let mut written_here: HashSet<String> = write_count.keys().cloned().collect();
    written_here.extend(let_declared.iter().cloned());
    for name in own_slots {
        // The body already went through `rename_reserved_words`: a slot named
        // `var` is read as `_var` there, so the declaration takes that form.
        let name = crate::util::sanitize_identifier(name);
        if !written_here.contains(&name)
            && !skip_decl(&name)
            && !free_closures.contains(&name)
            && !hoist.contains(&name)
            && is_valid_js_identifier(&name)
        {
            hoist.push(name);
        }
    }
    hoist.sort();
    hoist.dedup();
    if log::log_enabled!(target: "decl", log::Level::Debug) && !hoist.is_empty() {
        log::debug!(
            target: "decl",
            "top hoist {hoist:?} (let_declared {:?}, own_slots {:?})",
            let_declared.iter().collect::<Vec<_>>(),
            own_slots.iter().collect::<Vec<_>>()
        );
    }

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
            declared: &mut declared,
        },
    );
}

struct DeclSkip<'a> {
    outer_names: &'a HashSet<String>,
    declared: &'a mut HashSet<String>,
}

// One case does not see the names another case declared. Pattern targets
// (`{ kind: kind3 } = obj`) are declared at the top of that case, because
// they are emitted as a bare assignment.
fn declare_switch_case(
    body: &mut Vec<Statement>,
    write_count: &BTreeMap<String, usize>,
    let_declared: &HashSet<String>,
    params: &HashSet<&str>,
    free_closures: &HashSet<String>,
    skip: &mut DeclSkip<'_>,
) {
    let saved = skip.declared.clone();
    let mut names = Vec::new();
    collect_pattern_names(body, &mut names);
    names.sort();
    names.dedup();
    let mut prelude = Vec::new();
    for name in names {
        if params.contains(name.as_str())
            || let_declared.contains(&name)
            || saved.contains(&name)
            || skip.outer_names.contains(&name)
            || !is_valid_js_identifier(&name)
        {
            continue;
        }
        skip.declared.insert(name.clone());
        prelude.push(Statement::Let {
            name,
            value: Expression::constant(crate::ir::Constant::Undefined),
            kind: VarKind::Let,
        });
    }
    if !prelude.is_empty() {
        prelude.append(body);
        *body = prelude;
    }
    insert_decls_in_block(body, write_count, let_declared, params, free_closures, skip);
    *skip.declared = saved;
}

// The names bound by class and function declarations anywhere in `stmts`,
// nested blocks included (nested function bodies are other functions).
fn declaration_names(stmts: &[Statement]) -> HashSet<String> {
    use crate::ir::Visitor;
    struct C(HashSet<String>);
    impl<'a> Visitor<'a> for C {
        fn visit_statement(&mut self, s: &'a Statement) {
            match s {
                Statement::Class { name, .. } => {
                    self.0.insert(name.clone());
                }
                Statement::Let {
                    name,
                    value: Expression::Function { .. },
                    ..
                } => {
                    self.0.insert(name.clone());
                }
                _ => {}
            }
            self.walk_statement(s);
        }
    }
    let mut c = C(HashSet::new());
    for s in stmts {
        c.visit_statement(s);
    }
    c.0
}

fn is_env_slot_name(name: &str) -> bool {
    // closure_0, closure_1_2, c0, c12 — Hermes env / counter bindings
    if let Some(rest) = name.strip_prefix("closure_") {
        return !rest.is_empty()
            && rest.chars().all(|c| c.is_ascii_digit() || c == '_')
            && rest.chars().any(|c| c.is_ascii_digit());
    }
    name.len() >= 2 && name.starts_with('c') && name[1..].chars().all(|c| c.is_ascii_digit())
}

fn insert_decls_in_block(
    stmts: &mut Vec<Statement>,
    write_count: &BTreeMap<String, usize>,
    let_declared: &HashSet<String>,
    params: &HashSet<&str>,
    free_closures: &HashSet<String>,
    skip: &mut DeclSkip<'_>,
) {
    // A name assigned in a branch of this block and read after the branch
    // gets its `let` at the top of this block, whatever the nesting depth:
    // `let callResult = …` declared in a `then`, assigned in the `else` and
    // read after the `if` three levels down was bound nowhere the read could
    // see it.
    let declared_by_decl = declaration_names(stmts);
    let escaping: Vec<String> = names_assigned_in_nested_bodies_and_used_outside(stmts)
        .into_iter()
        .filter(|name| {
            is_valid_js_identifier(name)
                && !params.contains(name.as_str())
                && !let_declared.contains(name)
                && !skip.declared.contains(name)
                && !free_closures.contains(name)
                && !skip.outer_names.contains(name)
                && !declared_by_decl.contains(name)
        })
        .collect();
    if !escaping.is_empty() {
        log::debug!(target: "decl", "block hoist {escaping:?}");
        let mut decls: Vec<Statement> = escaping
            .iter()
            .map(|name| Statement::Let {
                name: name.clone(),
                value: Expression::constant(crate::ir::Constant::Undefined),
                kind: VarKind::Let,
            })
            .collect();
        for name in &escaping {
            skip.declared.insert(name.clone());
        }
        decls.append(stmts);
        *stmts = decls;
    }
    for stmt in stmts.iter_mut() {
        match stmt {
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Variable(name)),
                value,
            } => {
                // Skip invalid JS identifiers (numbers, strings, reserved words used as names)
                if is_valid_js_identifier(name)
                    && !params.contains(name.as_str())
                    && !let_declared.contains(name)
                    && !skip.declared.contains(name)
                    && !free_closures.contains(name)
                    && !skip.outer_names.contains(name)
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
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Register(r)),
                value,
            } => {
                let name = format!("r{r}");
                if !params.contains(name.as_str())
                    && !let_declared.contains(&name)
                    && !skip.declared.contains(&name)
                {
                    skip.declared.insert(name.clone());
                    let writes = write_count.get(&name).copied().unwrap_or(1);
                    let kind = if writes <= 1 {
                        VarKind::Const
                    } else {
                        VarKind::Let
                    };
                    *stmt = Statement::Let {
                        name,
                        value: value.clone(),
                        kind,
                    };
                }
            }
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                insert_decls_in_block(
                    then_body,
                    write_count,
                    let_declared,
                    params,
                    free_closures,
                    skip,
                );
                insert_decls_in_block(
                    else_body,
                    write_count,
                    let_declared,
                    params,
                    free_closures,
                    skip,
                );
            }
            Statement::While { body, .. }
            | Statement::DoWhile { body, .. }
            | Statement::For { body, .. }
            | Statement::ForIn { body, .. }
            | Statement::ForOf { body, .. } => {
                insert_decls_in_block(body, write_count, let_declared, params, free_closures, skip);
            }
            Statement::Block(inner) => {
                insert_decls_in_block(
                    inner,
                    write_count,
                    let_declared,
                    params,
                    free_closures,
                    skip,
                );
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                finally_body,
                ..
            } => {
                insert_decls_in_block(
                    try_body,
                    write_count,
                    let_declared,
                    params,
                    free_closures,
                    skip,
                );
                insert_decls_in_block(
                    catch_body,
                    write_count,
                    let_declared,
                    params,
                    free_closures,
                    skip,
                );
                insert_decls_in_block(
                    finally_body,
                    write_count,
                    let_declared,
                    params,
                    free_closures,
                    skip,
                );
            }
            Statement::Switch { cases, default, .. } => {
                for (_, body) in cases.iter_mut() {
                    declare_switch_case(
                        body,
                        write_count,
                        let_declared,
                        params,
                        free_closures,
                        skip,
                    );
                }
                if let Some(d) = default {
                    declare_switch_case(d, write_count, let_declared, params, free_closures, skip);
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
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

// Check if assignment is a self-assignment: `name = name`
fn is_self_assignment_var(name: &str, value: &Expression) -> bool {
    matches!(value, Expression::Value(Value::Binding(Binding::Variable(v))) if v == name)
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
