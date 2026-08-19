use crate::ir::{Expression, PropertyKey, Value};
use std::collections::BTreeMap;

use super::hints_tables::{is_generic_property, param_name_from_method, type_string_to_param_name};
use super::property_accesses::collect_param_property_accesses;

// Infer parameter names from how parameters are used in the function body.
// Returns a list of (param_index, suggested_name) pairs.
pub fn infer_param_names_from_body(stmts: &[crate::ir::Statement]) -> Vec<(u32, String)> {
    let mut hints: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for stmt in stmts {
        collect_body_param_hints_stmt(stmt, &mut hints);
    }

    // Post-pass: detect method receivers (params with 2+ unique property accesses)
    let mut prop_accesses: BTreeMap<u32, std::collections::HashSet<String>> = BTreeMap::new();
    collect_param_property_accesses(stmts, &mut prop_accesses);
    for (idx, props) in &prop_accesses {
        if props.len() >= 2 && !hints.contains_key(idx) {
            // The set of properties read on a parameter is ground truth: a param
            // read as `.email` and `.password` is a credentials object. When the
            // signature is recognized (email+password -> "user", latitude+longitude
            // -> "location", ...), use that concrete name; otherwise fall back to
            // the generic method-receiver name "self".
            let name = crate::analysis::naming::infer_type_from_properties(props)
                .map(str::to_string)
                .unwrap_or_else(|| "self".to_string());
            hints.entry(*idx).or_default().push(name);
        }
    }

    hints
        .into_iter()
        .filter_map(|(idx, names)| {
            // Prefer a semantic name (a property/error-derived identifier) over a
            // generic type name (`fn`/`arr`/`str`/...). Type names are only a
            // last resort, so `arg0.type` beats `arg0.map(...)` regardless of the
            // order they appear in the body.
            let chosen = names
                .iter()
                .find(|n| !super::is_type_fallback_name(n.as_str()))
                .or_else(|| names.first())
                .cloned();
            chosen.map(|n| (idx, n))
        })
        .collect()
}

fn collect_body_param_hints_stmt(
    stmt: &crate::ir::Statement,
    hints: &mut BTreeMap<u32, Vec<String>>,
) {
    use crate::ir::{AssignTarget, Statement};
    match stmt {
        Statement::Expr(e) => collect_body_param_hints_expr(e, hints),
        Statement::Assign { target, value } => {
            if let AssignTarget::Member { property, .. } = target {
                if let Expression::Value(Value::Parameter(idx)) = value {
                    if !is_generic_property(property) {
                        hints.entry(*idx).or_default().push(property.clone());
                    }
                }
            }
            collect_body_param_hints_target(target, hints);
            collect_body_param_hints_expr(value, hints);
        }
        Statement::Let { value, .. } => collect_body_param_hints_expr(value, hints),
        Statement::Return(Some(e)) | Statement::Throw(e) => {
            collect_body_param_hints_expr(e, hints)
        }
        Statement::If { condition, then_body, else_body } => {
            collect_body_param_hints_expr(condition, hints);
            for s in then_body { collect_body_param_hints_stmt(s, hints); }
            for s in else_body { collect_body_param_hints_stmt(s, hints); }
        }
        Statement::While { condition, body } | Statement::DoWhile { body, condition } => {
            collect_body_param_hints_expr(condition, hints);
            for s in body { collect_body_param_hints_stmt(s, hints); }
        }
        Statement::For { init, condition, update, body } => {
            if let Some(i) = init { collect_body_param_hints_stmt(i, hints); }
            if let Some(c) = condition { collect_body_param_hints_expr(c, hints); }
            if let Some(u) = update { collect_body_param_hints_stmt(u, hints); }
            for s in body { collect_body_param_hints_stmt(s, hints); }
        }
        Statement::ForIn { object, body, .. } => {
            if let Expression::Value(Value::Parameter(idx)) = object {
                hints.entry(*idx).or_default().push("obj".to_string());
            }
            collect_body_param_hints_expr(object, hints);
            for s in body { collect_body_param_hints_stmt(s, hints); }
        }
        Statement::ForOf { iterable, body, .. } => {
            if let Expression::Value(Value::Parameter(idx)) = iterable {
                hints.entry(*idx).or_default().push("items".to_string());
            }
            collect_body_param_hints_expr(iterable, hints);
            for s in body { collect_body_param_hints_stmt(s, hints); }
        }
        Statement::Block(inner) => {
            for s in inner { collect_body_param_hints_stmt(s, hints); }
        }
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
            for s in try_body { collect_body_param_hints_stmt(s, hints); }
            for s in catch_body { collect_body_param_hints_stmt(s, hints); }
            for s in finally_body { collect_body_param_hints_stmt(s, hints); }
        }
        Statement::Switch { discriminant, cases, default } => {
            collect_body_param_hints_expr(discriminant, hints);
            for (e, body) in cases {
                collect_body_param_hints_expr(e, hints);
                for s in body { collect_body_param_hints_stmt(s, hints); }
            }
            if let Some(d) = default {
                for s in d { collect_body_param_hints_stmt(s, hints); }
            }
        }
        _ => {}
    }
}

fn collect_body_param_hints_target(
    target: &crate::ir::AssignTarget,
    hints: &mut BTreeMap<u32, Vec<String>>,
) {
    use crate::ir::AssignTarget;
    match target {
        AssignTarget::Member { object, .. } => collect_body_param_hints_expr(object, hints),
        AssignTarget::Index { object, key } => {
            collect_body_param_hints_expr(object, hints);
            collect_body_param_hints_expr(key, hints);
        }
        _ => {}
    }
}

fn collect_body_param_hints_expr(expr: &Expression, hints: &mut BTreeMap<u32, Vec<String>>) {
    match expr {
        Expression::Call { callee, arguments } => {
            // A parameter invoked directly (`arg0(...)`) is unambiguously a function,
            // so fall back to the "fn" type name when nothing better is inferred.
            if let Expression::Value(Value::Parameter(idx)) = &**callee {
                hints.entry(*idx).or_default().push("fn".to_string());
            }
            if let Expression::Member { object, property: PropertyKey::Ident(method), .. } = &**callee {
                if let Expression::Value(Value::Parameter(idx)) = &**object {
                    if let Some(type_name) = param_name_from_method(method) {
                        hints.entry(*idx).or_default().push(type_name.to_string());
                    }
                }
                // Check for Array.isArray(arg)
                if method == "isArray" {
                    if let Expression::Value(Value::Variable(name)) = &**object {
                        if name == "Array" {
                            if let Some(Expression::Value(Value::Parameter(idx))) = arguments.first() {
                                hints.entry(*idx).or_default().push("arr".to_string());
                            }
                        }
                    }
                }
            }
            collect_body_param_hints_expr(callee, hints);
            for a in arguments { collect_body_param_hints_expr(a, hints); }
        }
        Expression::Member { object, property: PropertyKey::Ident(prop), .. } => {
            if let Expression::Value(Value::Parameter(idx)) = &**object {
                if !is_generic_property(prop) && param_name_from_method(prop).is_none() {
                    hints.entry(*idx).or_default().push(prop.clone());
                }
            }
            collect_body_param_hints_expr(object, hints);
        }
        Expression::Binary { left, right, .. } => {
            if let Expression::Unary { op: crate::ir::UnaryOp::TypeOf, operand } = &**left {
                if let Expression::Value(Value::Parameter(idx)) = &**operand {
                    if let Expression::Value(Value::Constant(crate::ir::Constant::String(s))) = &**right {
                        if let Some(name) = type_string_to_param_name(s) {
                            hints.entry(*idx).or_default().push(name.to_string());
                        }
                    }
                }
            }
            collect_body_param_hints_expr(left, hints);
            collect_body_param_hints_expr(right, hints);
        }
        Expression::New { callee, arguments } => {
            collect_body_param_hints_expr(callee, hints);
            for a in arguments { collect_body_param_hints_expr(a, hints); }
        }
        Expression::Unary { operand, .. } => collect_body_param_hints_expr(operand, hints),
        Expression::Conditional { condition, then_expr, else_expr } => {
            collect_body_param_hints_expr(condition, hints);
            collect_body_param_hints_expr(then_expr, hints);
            collect_body_param_hints_expr(else_expr, hints);
        }
        Expression::Array { elements } => {
            for e in elements.iter().flatten() { collect_body_param_hints_expr(e, hints); }
        }
        Expression::Object { properties } => {
            for p in properties { collect_body_param_hints_expr(&p.value, hints); }
        }
        Expression::Assignment { target, value } => {
            collect_body_param_hints_expr(target, hints);
            collect_body_param_hints_expr(value, hints);
        }
        Expression::Spread(inner) | Expression::Await(inner) => collect_body_param_hints_expr(inner, hints),
        Expression::Yield { value, .. } => collect_body_param_hints_expr(value, hints),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{PropertyKey, Statement};

    fn member(idx: u32, prop: &str) -> Expression {
        Expression::Member {
            object: Box::new(Expression::Value(Value::Parameter(idx))),
            property: PropertyKey::Ident(prop.to_string()),
            optional: false,
        }
    }

    // A semantic property name must win over a generic type name derived from a
    // method call, regardless of the order they appear in the body.
    #[test]
    fn semantic_property_beats_type_name() {
        // arg0.map(...) appears before arg0.type
        let stmts = vec![
            Statement::Expr(Expression::Call {
                callee: Box::new(member(0, "map")),
                arguments: vec![],
            }),
            Statement::Return(Some(member(0, "type"))),
        ];
        let hints = infer_param_names_from_body(&stmts);
        assert_eq!(hints, vec![(0, "type".to_string())]);
    }

    // A parameter invoked directly is named with the "fn" type fallback when no
    // better name is available.
    #[test]
    fn called_parameter_named_fn() {
        let stmts = vec![Statement::Expr(Expression::Call {
            callee: Box::new(Expression::Value(Value::Parameter(0))),
            arguments: vec![Expression::Value(Value::Parameter(1))],
        })];
        let hints = infer_param_names_from_body(&stmts);
        assert!(hints.contains(&(0, "fn".to_string())));
    }
}
