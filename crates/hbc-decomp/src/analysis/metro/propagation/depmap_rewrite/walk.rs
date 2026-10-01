use super::super::super::registry::FactoryRoles;
use super::super::is_dep_array_name;
use crate::ir::{
    map_nested_bodies_mut, AssignTarget, Binding, Expression, PropertyKey, Statement, Value,
};
use std::collections::HashSet;

pub(super) fn rewrite_stmts(
    stmts: &mut [Statement],
    deps: &[u32],
    aliases: &mut HashSet<String>,
    roles: &FactoryRoles,
) -> u64 {
    let mut count = 0u64;
    for stmt in stmts.iter_mut() {
        count += rewrite_stmt(stmt, deps, aliases, roles);
    }
    count
}

fn rewrite_stmt(
    stmt: &mut Statement,
    deps: &[u32],
    aliases: &mut HashSet<String>,
    roles: &FactoryRoles,
) -> u64 {
    let mut count = 0u64;
    // Track new aliases as we go (single-pass, order-sensitive, good enough).
    match stmt {
        Statement::Let { name, value, .. } => {
            count += rewrite_expr(value, deps, aliases, roles);
            if super::expr_is_depmap_root(value, aliases, roles) {
                aliases.insert(name.clone());
            }
        }
        Statement::Assign { target, value } => {
            count += rewrite_expr(value, deps, aliases, roles);
            count += rewrite_target(target, deps, aliases, roles);
            if let AssignTarget::Binding(Binding::Variable(name)) = target {
                if super::expr_is_depmap_root(value, aliases, roles) {
                    aliases.insert(name.clone());
                }
            }
        }
        Statement::Delete { target, .. } => {
            count += rewrite_expr(target, deps, aliases, roles);
        }
        Statement::Expr(e) | Statement::Throw(e) => {
            count += rewrite_expr(e, deps, aliases, roles);
        }
        Statement::Return(Some(e)) => {
            count += rewrite_expr(e, deps, aliases, roles);
        }
        Statement::If { condition, .. } => {
            count += rewrite_expr(condition, deps, aliases, roles);
        }
        Statement::While { condition, .. } | Statement::DoWhile { condition, .. } => {
            count += rewrite_expr(condition, deps, aliases, roles);
        }
        Statement::For {
            condition,
            init,
            update,
            ..
        } => {
            if let Some(c) = condition {
                count += rewrite_expr(c, deps, aliases, roles);
            }
            if let Some(i) = init {
                count += rewrite_stmt(i, deps, aliases, roles);
            }
            if let Some(u) = update {
                count += rewrite_stmt(u, deps, aliases, roles);
            }
        }
        Statement::ForIn { object, .. } => {
            count += rewrite_expr(object, deps, aliases, roles);
        }
        Statement::ForOf { iterable, .. } => {
            count += rewrite_expr(iterable, deps, aliases, roles);
        }
        Statement::Switch {
            discriminant,
            cases,
            ..
        } => {
            count += rewrite_expr(discriminant, deps, aliases, roles);
            for (val, _) in cases.iter_mut() {
                count += rewrite_expr(val, deps, aliases, roles);
            }
        }
        Statement::Class {
            super_class,
            constructor,
            methods,
            ..
        } => {
            if let Some(s) = super_class {
                count += rewrite_expr(s, deps, aliases, roles);
            }
            if let Some(c) = constructor {
                count += rewrite_stmt(c, deps, aliases, roles);
            }
            for m in methods.iter_mut() {
                count += rewrite_expr(&mut m.value, deps, aliases, roles);
            }
        }
        Statement::CondGoto { condition, .. } => {
            count += rewrite_expr(condition, deps, aliases, roles);
        }
        _ => {}
    }

    map_nested_bodies_mut(stmt, |body| {
        let mut body = body;
        count += rewrite_stmts(&mut body, deps, aliases, roles);
        body
    });
    count
}

fn rewrite_target(
    target: &mut AssignTarget,
    deps: &[u32],
    aliases: &HashSet<String>,
    roles: &FactoryRoles,
) -> u64 {
    match target {
        AssignTarget::Member { object, .. } => rewrite_expr(object, deps, aliases, roles),
        AssignTarget::Index { object, key } => {
            rewrite_expr(object, deps, aliases, roles) + rewrite_expr(key, deps, aliases, roles)
        }
        _ => 0,
    }
}

fn rewrite_expr(
    expr: &mut Expression,
    deps: &[u32],
    aliases: &HashSet<String>,
    roles: &FactoryRoles,
) -> u64 {
    if let Some(mod_id) = try_resolve_depmap_index(expr, deps, aliases, roles) {
        log::trace!(target: "depmap", "rewrite {expr:?} -> module {mod_id}");
        *expr = Expression::Value(Value::Constant(crate::ir::Constant::Integer(mod_id as i32)));
        return 1;
    }

    match expr {
        Expression::Binary { left, right, .. } => {
            rewrite_expr(left, deps, aliases, roles) + rewrite_expr(right, deps, aliases, roles)
        }
        Expression::Unary { operand, .. } => rewrite_expr(operand, deps, aliases, roles),
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            let mut c = rewrite_expr(callee, deps, aliases, roles);
            for a in arguments.iter_mut() {
                c += rewrite_expr(a, deps, aliases, roles);
            }
            c
        }
        Expression::Member {
            object, property, ..
        } => {
            let mut c = rewrite_expr(object, deps, aliases, roles);
            if let PropertyKey::Computed(key) = property {
                c += rewrite_expr(key, deps, aliases, roles);
            }
            c
        }
        Expression::Object { properties } => {
            let mut c = 0u64;
            for p in properties.iter_mut() {
                // Computed keys can embed require(dependencyMap[k]).Foo, rewrite them too.
                if let PropertyKey::Computed(key) = &mut p.key {
                    c += rewrite_expr(key, deps, aliases, roles);
                }
                c += rewrite_expr(&mut p.value, deps, aliases, roles);
            }
            c
        }
        Expression::Array { elements } => {
            let mut c = 0u64;
            for e in elements.iter_mut().flatten() {
                c += rewrite_expr(e, deps, aliases, roles);
            }
            c
        }
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            rewrite_expr(condition, deps, aliases, roles)
                + rewrite_expr(then_expr, deps, aliases, roles)
                + rewrite_expr(else_expr, deps, aliases, roles)
        }
        Expression::Assignment { target, value } => {
            let mut n = 0u64;
            crate::ir::for_each_target_expression_mut(target, &mut |e| {
                n += rewrite_expr(e, deps, aliases, roles)
            });
            n + rewrite_expr(value, deps, aliases, roles)
        }
        Expression::TemplateLiteral { expressions, .. } => {
            let mut c = 0u64;
            for e in expressions.iter_mut() {
                c += rewrite_expr(e, deps, aliases, roles);
            }
            c
        }
        Expression::Yield { value, .. } => rewrite_expr(value, deps, aliases, roles),
        Expression::Await(e) | Expression::Spread(e) => rewrite_expr(e, deps, aliases, roles),
        Expression::JSXElement {
            attributes,
            children,
            ..
        } => {
            let mut c = 0u64;
            for (_, v) in attributes.iter_mut() {
                c += rewrite_expr(v, deps, aliases, roles);
            }
            for ch in children.iter_mut() {
                c += rewrite_expr(ch, deps, aliases, roles);
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
    roles: &FactoryRoles,
) -> Option<u32> {
    let Expression::Member {
        object, property, ..
    } = expr
    else {
        return None;
    };

    let base_is_dep = match object.as_ref() {
        Expression::Value(Value::Binding(Binding::Variable(name))) => {
            aliases.contains(name) || is_dep_array_name(name, roles)
        }
        Expression::Value(Value::Parameter(idx)) => roles.deps_idx == Some(*idx),
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
