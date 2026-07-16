// Rewrite `dependencyMap[N]` / aliases into absolute module IDs.
//
// Metro factories receive a dependency array as their last parameter. The
// compiler lowers `require('./foo')` to `require(dependencyMap[k])` where `k`
// indexes that factory's dependency list.
//
// This pass MUST run AFTER `resolve_closures`: nested functions capture the
// map as a ClosureVar, which only becomes `Variable("dependencyMap")` (or an
// alias) once slots are resolved. Running earlier leaves ~half the references
// unresolved (Discord HBC96 baseline: 162k → needs late rewrite).

use super::super::registry::MetroRegistry;
use super::{default_roles, is_dep_array_name};
use crate::analysis::ClosureContext;
use crate::ir::{
    map_nested_bodies_mut, AssignTarget, Expression, PropertyKey, Statement, Value,
};
use std::collections::{BTreeMap, HashSet};

pub fn rewrite_dependency_map_indices(
    functions: &mut BTreeMap<u32, Vec<Statement>>,
    registry: &MetroRegistry,
    closure_ctx: &Option<ClosureContext>,
) {
    // factory function_id → dependency list
    let mut factory_deps: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for module in registry.modules.values() {
        if !module.dependencies.is_empty() {
            factory_deps.insert(module.function_id, module.dependencies.clone());
        }
    }
    for (func_id, mod_id) in &registry.function_to_module {
        if !factory_deps.contains_key(func_id) {
            if let Some(module) = registry.modules.get(mod_id) {
                if !module.dependencies.is_empty() {
                    factory_deps.insert(*func_id, module.dependencies.clone());
                }
            }
        }
    }
    if factory_deps.is_empty() {
        return;
    }

    // Resolve deps for every function via parent_function chain.
    let mut func_deps: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let ids: Vec<u32> = functions.keys().copied().collect();
    for fid in &ids {
        if let Some(deps) = factory_deps.get(fid) {
            func_deps.insert(*fid, deps.clone());
            continue;
        }
        if let Some(ctx) = closure_ctx {
            let mut current = *fid;
            let mut seen = HashSet::new();
            seen.insert(current);
            while let Some(&parent) = ctx.parent_function.get(&current) {
                if !seen.insert(parent) {
                    break;
                }
                if let Some(deps) = factory_deps.get(&parent) {
                    func_deps.insert(*fid, deps.clone());
                    break;
                }
                current = parent;
            }
        }
    }

    let mut rewrites = 0u64;
    for fid in ids {
        let Some(deps) = func_deps.get(&fid).cloned() else {
            continue;
        };
        if let Some(stmts) = functions.get_mut(&fid) {
            // Collect local aliases of the dependency array (e.g.
            // `const map = dependencyMap`, `let d = arg4`).
            let mut aliases = collect_depmap_aliases(stmts);
            rewrites += rewrite_stmts(stmts, &deps, &mut aliases);
        }
    }
    if rewrites > 0 {
        log::debug!("[metro] late-rewrote {rewrites} dependencyMap[N] indices to absolute module IDs");
    }
}

// Names known to refer to the factory dependency array.
fn collect_depmap_aliases(stmts: &[Statement]) -> HashSet<String> {
    let mut aliases: HashSet<String> = HashSet::new();
    // Always recognize the canonical names (including names we rewrite
    // factory-captured slots to via get_slot_name).
    aliases.insert("dependencyMap".into());
    aliases.insert("deps".into());
    aliases.insert("_dependencyMap".into());
    // After closure naming, heavily-indexed captures may still be called
    // `dependencyMap` or a unique `dependencyMap2`, handled below by prefix.

    fn walk(stmts: &[Statement], aliases: &mut HashSet<String>) {
        for stmt in stmts {
            match stmt {
                Statement::Let { name, value, .. } | Statement::Assign {
                    target: AssignTarget::Variable(name),
                    value,
                } => {
                    if expr_is_depmap_root(value, aliases) {
                        aliases.insert(name.clone());
                    }
                }
                Statement::Assign {
                    target: AssignTarget::Register(r),
                    value,
                } => {
                    if expr_is_depmap_root(value, aliases) {
                        aliases.insert(format!("r{r}"));
                    }
                }
                _ => {}
            }
            // Nested bodies: re-scan with same alias set (conservative, aliases
            // from outer scopes are valid inside).
            let mut nested: Vec<&[Statement]> = Vec::new();
            match stmt {
                Statement::If {
                    then_body, else_body, ..
                } => {
                    nested.push(then_body);
                    nested.push(else_body);
                }
                Statement::While { body, .. }
                | Statement::DoWhile { body, .. }
                | Statement::For { body, .. }
                | Statement::ForIn { body, .. }
                | Statement::ForOf { body, .. }
                | Statement::Block(body) => nested.push(body),
                Statement::TryCatch {
                    try_body,
                    catch_body,
                    finally_body,
                    ..
                } => {
                    nested.push(try_body);
                    nested.push(catch_body);
                    nested.push(finally_body);
                }
                Statement::Switch { cases, default, .. } => {
                    for (_, b) in cases {
                        nested.push(b);
                    }
                    if let Some(d) = default {
                        nested.push(d);
                    }
                }
                _ => {}
            }
            for n in nested {
                walk(n, aliases);
            }
        }
    }
    walk(stmts, &mut aliases);
    aliases
}

fn expr_is_depmap_root(expr: &Expression, aliases: &HashSet<String>) -> bool {
    match expr {
        Expression::Value(Value::Variable(n)) => {
            aliases.contains(n) || is_dep_array_name(n, &default_roles())
        }
        Expression::Value(Value::Parameter(idx)) => {
            let roles = default_roles();
            roles.deps_idx == Some(*idx) || *idx >= 4
        }
        _ => false,
    }
}

fn rewrite_stmts(
    stmts: &mut [Statement],
    deps: &[u32],
    aliases: &mut HashSet<String>,
) -> u64 {
    let mut count = 0u64;
    for stmt in stmts.iter_mut() {
        count += rewrite_stmt(stmt, deps, aliases);
    }
    count
}

fn rewrite_stmt(
    stmt: &mut Statement,
    deps: &[u32],
    aliases: &mut HashSet<String>,
) -> u64 {
    let mut count = 0u64;
    // Track new aliases as we go (single-pass, order-sensitive, good enough).
    match stmt {
        Statement::Let { name, value, .. } => {
            count += rewrite_expr(value, deps, aliases);
            if expr_is_depmap_root(value, aliases) {
                aliases.insert(name.clone());
            }
        }
        Statement::Assign { target, value } => {
            count += rewrite_expr(value, deps, aliases);
            count += rewrite_target(target, deps, aliases);
            if let AssignTarget::Variable(name) = target {
                if expr_is_depmap_root(value, aliases) {
                    aliases.insert(name.clone());
                }
            }
        }
        Statement::Delete { target, .. } => {
            count += rewrite_expr(target, deps, aliases);
        }
        Statement::Expr(e) | Statement::Throw(e) => {
            count += rewrite_expr(e, deps, aliases);
        }
        Statement::Return(Some(e)) => {
            count += rewrite_expr(e, deps, aliases);
        }
        Statement::If { condition, .. } => {
            count += rewrite_expr(condition, deps, aliases);
        }
        Statement::While { condition, .. } | Statement::DoWhile { condition, .. } => {
            count += rewrite_expr(condition, deps, aliases);
        }
        Statement::For {
            condition,
            init,
            update,
            ..
        } => {
            if let Some(c) = condition {
                count += rewrite_expr(c, deps, aliases);
            }
            if let Some(i) = init {
                count += rewrite_stmt(i, deps, aliases);
            }
            if let Some(u) = update {
                count += rewrite_stmt(u, deps, aliases);
            }
        }
        Statement::ForIn { object, .. } => {
            count += rewrite_expr(object, deps, aliases);
        }
        Statement::ForOf { iterable, .. } => {
            count += rewrite_expr(iterable, deps, aliases);
        }
        Statement::Switch {
            discriminant,
            cases,
            ..
        } => {
            count += rewrite_expr(discriminant, deps, aliases);
            for (val, _) in cases.iter_mut() {
                count += rewrite_expr(val, deps, aliases);
            }
        }
        Statement::Class {
            super_class,
            constructor,
            methods,
            ..
        } => {
            if let Some(s) = super_class {
                count += rewrite_expr(s, deps, aliases);
            }
            if let Some(c) = constructor {
                count += rewrite_stmt(c, deps, aliases);
            }
            for m in methods.iter_mut() {
                count += rewrite_expr(&mut m.value, deps, aliases);
            }
        }
        Statement::CondGoto { condition, .. } => {
            count += rewrite_expr(condition, deps, aliases);
        }
        _ => {}
    }

    map_nested_bodies_mut(stmt, |body| {
        let mut body = body;
        count += rewrite_stmts(&mut body, deps, aliases);
        body
    });
    count
}

fn rewrite_target(
    target: &mut AssignTarget,
    deps: &[u32],
    aliases: &HashSet<String>,
) -> u64 {
    match target {
        AssignTarget::Member { object, .. } => rewrite_expr(object, deps, aliases),
        AssignTarget::Index { object, key } => {
            rewrite_expr(object, deps, aliases) + rewrite_expr(key, deps, aliases)
        }
        _ => 0,
    }
}

fn rewrite_expr(
    expr: &mut Expression,
    deps: &[u32],
    aliases: &HashSet<String>,
) -> u64 {
    if let Some(mod_id) = try_resolve_depmap_index(expr, deps, aliases) {
        *expr = Expression::Value(Value::Constant(crate::ir::Constant::Integer(mod_id as i32)));
        return 1;
    }

    match expr {
        Expression::Binary { left, right, .. } => {
            rewrite_expr(left, deps, aliases) + rewrite_expr(right, deps, aliases)
        }
        Expression::Unary { operand, .. } => rewrite_expr(operand, deps, aliases),
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            let mut c = rewrite_expr(callee, deps, aliases);
            for a in arguments.iter_mut() {
                c += rewrite_expr(a, deps, aliases);
            }
            c
        }
        Expression::Member { object, property, .. } => {
            let mut c = rewrite_expr(object, deps, aliases);
            if let PropertyKey::Computed(key) = property {
                c += rewrite_expr(key, deps, aliases);
            }
            c
        }
        Expression::Object { properties } => {
            let mut c = 0u64;
            for p in properties.iter_mut() {
                // Computed keys can embed require(dependencyMap[k]).Foo, rewrite them too.
                if let PropertyKey::Computed(key) = &mut p.key {
                    c += rewrite_expr(key, deps, aliases);
                }
                c += rewrite_expr(&mut p.value, deps, aliases);
            }
            c
        }
        Expression::Array { elements } => {
            let mut c = 0u64;
            for e in elements.iter_mut().flatten() {
                c += rewrite_expr(e, deps, aliases);
            }
            c
        }
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            rewrite_expr(condition, deps, aliases)
                + rewrite_expr(then_expr, deps, aliases)
                + rewrite_expr(else_expr, deps, aliases)
        }
        Expression::Assignment { target, value } => {
            rewrite_expr(target, deps, aliases) + rewrite_expr(value, deps, aliases)
        }
        Expression::TemplateLiteral { expressions, .. } => {
            let mut c = 0u64;
            for e in expressions.iter_mut() {
                c += rewrite_expr(e, deps, aliases);
            }
            c
        }
        Expression::Yield { value, .. } => rewrite_expr(value, deps, aliases),
        Expression::Await(e) | Expression::Spread(e) => rewrite_expr(e, deps, aliases),
        Expression::JSXElement {
            attributes,
            children,
            ..
        } => {
            let mut c = 0u64;
            for (_, v) in attributes.iter_mut() {
                c += rewrite_expr(v, deps, aliases);
            }
            for ch in children.iter_mut() {
                c += rewrite_expr(ch, deps, aliases);
            }
            c
        }
        Expression::Value(_)
        | Expression::Function { .. }
        | Expression::RegExp { .. }
        | Expression::Unknown { .. } => 0,
    }
}

fn try_resolve_depmap_index(
    expr: &Expression,
    deps: &[u32],
    aliases: &HashSet<String>,
) -> Option<u32> {
    let Expression::Member {
        object, property, ..
    } = expr
    else {
        return None;
    };

    let base_is_dep = match object.as_ref() {
        Expression::Value(Value::Variable(name)) => {
            aliases.contains(name)
                || is_dep_array_name(name, &default_roles())
                // Unique-ified names: dependencyMap2, deps3, …
                || name.starts_with("dependencyMap")
                || (name.starts_with("deps") && name[4..].chars().all(|c| c.is_ascii_digit()))
        }
        Expression::Value(Value::Parameter(idx)) => {
            let roles = default_roles();
            // Accept any trailing param that could be the dependency array
            // (classic Metro: 4, modern with helpers: 6).
            roles.deps_idx == Some(*idx) || *idx >= 4
        }
        _ => false,
    };
    if !base_is_dep {
        return None;
    }

    let idx = match property {
        PropertyKey::Index(i) if *i >= 0 => *i as u32,
        PropertyKey::Computed(key) => match key.as_ref() {
            Expression::Value(Value::Constant(crate::ir::Constant::Integer(i))) if *i >= 0 => {
                *i as u32
            }
            // Register/variable index, can't resolve statically.
            _ => return None,
        },
        _ => return None,
    };

    deps.get(idx as usize).copied()
}
