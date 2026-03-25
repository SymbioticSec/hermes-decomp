use crate::analysis::metro::registry::FactoryRoles;
use crate::ir::{Expression, PropertyKey, Value};
use std::collections::BTreeMap;

use super::hints_tables::is_generic_property;

// Count unique property accesses per parameter across the function body.
pub(super) fn collect_param_property_accesses(
    stmts: &[crate::ir::Statement],
    accesses: &mut BTreeMap<u32, std::collections::HashSet<String>>,
) {
    for stmt in stmts {
        collect_param_props_stmt(stmt, accesses);
    }
}

fn collect_param_props_stmt(
    stmt: &crate::ir::Statement,
    accesses: &mut BTreeMap<u32, std::collections::HashSet<String>>,
) {
    use crate::ir::Statement;
    match stmt {
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
            collect_param_props_expr(e, accesses)
        }
        Statement::Assign { target, value } => {
            if let crate::ir::AssignTarget::Member { object, .. } = target {
                collect_param_props_expr(object, accesses);
            }
            collect_param_props_expr(value, accesses);
        }
        Statement::Let { value, .. } => collect_param_props_expr(value, accesses),
        Statement::If { condition, then_body, else_body } => {
            collect_param_props_expr(condition, accesses);
            collect_param_property_accesses(then_body, accesses);
            collect_param_property_accesses(else_body, accesses);
        }
        Statement::While { condition, body } | Statement::DoWhile { body, condition } => {
            collect_param_props_expr(condition, accesses);
            collect_param_property_accesses(body, accesses);
        }
        Statement::For { init, condition, update, body } => {
            if let Some(s) = init { collect_param_props_stmt(s, accesses); }
            if let Some(e) = condition { collect_param_props_expr(e, accesses); }
            if let Some(s) = update { collect_param_props_stmt(s, accesses); }
            collect_param_property_accesses(body, accesses);
        }
        Statement::ForIn { object, body, .. } => {
            collect_param_props_expr(object, accesses);
            collect_param_property_accesses(body, accesses);
        }
        Statement::ForOf { iterable, body, .. } => {
            collect_param_props_expr(iterable, accesses);
            collect_param_property_accesses(body, accesses);
        }
        Statement::Block(inner) => collect_param_property_accesses(inner, accesses),
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
            collect_param_property_accesses(try_body, accesses);
            collect_param_property_accesses(catch_body, accesses);
            collect_param_property_accesses(finally_body, accesses);
        }
        Statement::Switch { discriminant, cases, default } => {
            collect_param_props_expr(discriminant, accesses);
            for (e, body) in cases {
                collect_param_props_expr(e, accesses);
                collect_param_property_accesses(body, accesses);
            }
            if let Some(d) = default { collect_param_property_accesses(d, accesses); }
        }
        _ => {}
    }
}

fn collect_param_props_expr(
    expr: &Expression,
    accesses: &mut BTreeMap<u32, std::collections::HashSet<String>>,
) {
    match expr {
        Expression::Member { object, property: PropertyKey::Ident(prop), .. } => {
            let param_idx = match &**object {
                Expression::Value(Value::Parameter(idx)) => Some(*idx),
                Expression::Value(Value::Variable(name)) => FactoryRoles::extract_param_index(name),
                _ => None,
            };
            if let Some(idx) = param_idx {
                if !is_generic_property(prop) {
                    accesses.entry(idx).or_default().insert(prop.clone());
                }
            }
            collect_param_props_expr(object, accesses);
        }
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            collect_param_props_expr(callee, accesses);
            for a in arguments { collect_param_props_expr(a, accesses); }
        }
        Expression::Binary { left, right, .. } => {
            collect_param_props_expr(left, accesses);
            collect_param_props_expr(right, accesses);
        }
        Expression::Unary { operand, .. } => collect_param_props_expr(operand, accesses),
        Expression::Conditional { condition, then_expr, else_expr } => {
            collect_param_props_expr(condition, accesses);
            collect_param_props_expr(then_expr, accesses);
            collect_param_props_expr(else_expr, accesses);
        }
        Expression::Array { elements } => {
            for e in elements.iter().flatten() { collect_param_props_expr(e, accesses); }
        }
        Expression::Object { properties } => {
            for p in properties { collect_param_props_expr(&p.value, accesses); }
        }
        Expression::Assignment { target, value } => {
            collect_param_props_expr(target, accesses);
            collect_param_props_expr(value, accesses);
        }
        Expression::Spread(inner) | Expression::Await(inner) => collect_param_props_expr(inner, accesses),
        Expression::Yield { value, .. } => collect_param_props_expr(value, accesses),
        _ => {}
    }
}
