use super::super::super::registry::{FactoryRoles, MetroRegistry};
use crate::analysis::ClosureContext;
use crate::ir::{
    map_nested_bodies_mut, AssignTarget, Binding, Expression, PropertyKey, Statement, Value,
};
use std::collections::{BTreeMap, HashSet};

pub(super) fn require_callee_names(
    fid: u32,
    roles: &FactoryRoles,
    param_names: &BTreeMap<u32, Vec<Option<String>>>,
    closure_ctx: &Option<ClosureContext>,
    registry: &MetroRegistry,
) -> HashSet<String> {
    let mut names = HashSet::new();
    names.insert("require".to_string());
    let mut factory = fid;
    if let Some(ctx) = closure_ctx {
        let mut current = fid;
        let mut seen = HashSet::new();
        seen.insert(current);
        while let Some(&parent) = ctx.parent_function.get(&current) {
            if !seen.insert(parent) {
                break;
            }
            if registry.function_to_module.contains_key(&parent) {
                factory = parent;
                break;
            }
            current = parent;
        }
    }
    if registry.function_to_module.contains_key(&fid) {
        factory = fid;
    }
    if let Some(params) = param_names.get(&factory) {
        if let Some(Some(name)) = params.get(roles.require_idx as usize) {
            if !name.is_empty() {
                names.insert(name.clone());
            }
        }
    }
    names
}

pub(super) fn rewrite_require_dep_calls(
    stmts: &mut [Statement],
    deps: &[u32],
    loose: &HashSet<String>,
    require_names: &HashSet<String>,
    roles: &FactoryRoles,
) -> u64 {
    let mut count = 0u64;
    for stmt in stmts.iter_mut() {
        count += rewrite_require_dep_stmt(stmt, deps, loose, require_names, roles);
    }
    count
}

fn rewrite_require_dep_stmt(
    stmt: &mut Statement,
    deps: &[u32],
    loose: &HashSet<String>,
    require_names: &HashSet<String>,
    roles: &FactoryRoles,
) -> u64 {
    let mut count = 0u64;
    match stmt {
        Statement::Let { value, .. }
        | Statement::Expr(value)
        | Statement::Throw(value)
        | Statement::Return(Some(value)) => {
            count += rewrite_require_dep_expr(value, deps, loose, require_names, roles);
        }
        Statement::Assign { value, target } => {
            count += rewrite_require_dep_expr(value, deps, loose, require_names, roles);
            if let AssignTarget::Member { object, .. } = target {
                count += rewrite_require_dep_expr(object, deps, loose, require_names, roles);
            }
        }
        Statement::If { condition, .. }
        | Statement::While { condition, .. }
        | Statement::DoWhile { condition, .. } => {
            count += rewrite_require_dep_expr(condition, deps, loose, require_names, roles);
        }
        Statement::ForOf { iterable, .. } => {
            count += rewrite_require_dep_expr(iterable, deps, loose, require_names, roles);
        }
        _ => {}
    }
    map_nested_bodies_mut(stmt, |body| {
        let mut body = body;
        count += rewrite_require_dep_calls(&mut body, deps, loose, require_names, roles);
        body
    });
    count
}

fn rewrite_require_dep_expr(
    expr: &mut Expression,
    deps: &[u32],
    loose: &HashSet<String>,
    require_names: &HashSet<String>,
    roles: &FactoryRoles,
) -> u64 {
    let mut count = 0u64;
    if let Expression::Call { callee, arguments } = expr {
        if callee_is_require(callee, require_names, roles) {
            for arg in arguments.iter_mut() {
                if let Some(id) = loose_dep_index(arg, deps, loose) {
                    *arg =
                        Expression::Value(Value::Constant(crate::ir::Constant::Integer(id as i32)));
                    count += 1;
                }
            }
        }
    }
    match expr {
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            count += rewrite_require_dep_expr(callee, deps, loose, require_names, roles);
            for arg in arguments {
                count += rewrite_require_dep_expr(arg, deps, loose, require_names, roles);
            }
        }
        Expression::Member { object, .. }
        | Expression::Unary {
            operand: object, ..
        } => {
            count += rewrite_require_dep_expr(object, deps, loose, require_names, roles);
        }
        Expression::Binary { left, right, .. } => {
            count += rewrite_require_dep_expr(left, deps, loose, require_names, roles);
            count += rewrite_require_dep_expr(right, deps, loose, require_names, roles);
        }
        _ => {}
    }
    count
}

fn callee_is_require(
    callee: &Expression,
    require_names: &HashSet<String>,
    roles: &FactoryRoles,
) -> bool {
    match callee {
        Expression::Value(Value::Binding(Binding::Variable(name))) => require_names.contains(name),
        Expression::Value(Value::Parameter(idx)) => *idx == roles.require_idx,
        _ => false,
    }
}

fn loose_dep_index(expr: &Expression, deps: &[u32], loose: &HashSet<String>) -> Option<u32> {
    let Expression::Member {
        object, property, ..
    } = expr
    else {
        return None;
    };
    let Expression::Value(Value::Binding(Binding::Variable(name))) = object.as_ref() else {
        return None;
    };
    if !loose.contains(name) {
        return None;
    }
    let idx = match property {
        PropertyKey::Index(i) if *i >= 0 => *i as u32,
        PropertyKey::Computed(key) => match key.as_ref() {
            Expression::Value(Value::Constant(crate::ir::Constant::Integer(i))) if *i >= 0 => {
                *i as u32
            }
            _ => return None,
        },
        _ => return None,
    };
    deps.get(idx as usize).copied()
}

// Names from `aliases` that are assigned a value which is not the dependency
// array, anywhere in the function.
