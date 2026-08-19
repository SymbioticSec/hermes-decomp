use crate::ir::{AssignTarget, Statement};

// Collect destructuring-pattern target names across the whole function.
pub(super) fn collect_pattern_names(stmts: &[Statement], out: &mut Vec<String>) {
    for stmt in stmts {
        if let Statement::Assign { target, .. } = stmt {
            if matches!(
                target,
                AssignTarget::DestructuringArray(_)
                    | AssignTarget::DestructuringArrayRest { .. }
                    | AssignTarget::DestructuringObject(_)
                    | AssignTarget::DestructuringObjectRest { .. }
            ) {
                out.extend(destructuring_target_names(target));
            }
        }
        match stmt {
            Statement::If { then_body, else_body, .. } => {
                collect_pattern_names(then_body, out);
                collect_pattern_names(else_body, out);
            }
            Statement::While { body, .. } | Statement::DoWhile { body, .. }
            | Statement::For { body, .. } | Statement::ForIn { body, .. }
            | Statement::ForOf { body, .. } => collect_pattern_names(body, out),
            Statement::Block(inner) => collect_pattern_names(inner, out),
            Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
                collect_pattern_names(try_body, out);
                collect_pattern_names(catch_body, out);
                collect_pattern_names(finally_body, out);
            }
            Statement::Switch { cases, default, .. } => {
                for (_, body) in cases {
                    collect_pattern_names(body, out);
                }
                if let Some(d) = default {
                    collect_pattern_names(d, out);
                }
            }
            _ => {}
        }
    }
}

// Variable names bound by a destructuring assignment target.
pub(super) fn destructuring_target_names(target: &AssignTarget) -> Vec<String> {
    let mut out = Vec::new();
    fn add(t: &AssignTarget, out: &mut Vec<String>) {
        match t {
            AssignTarget::Variable(n) => out.push(n.clone()),
            AssignTarget::DestructuringArray(elems) => {
                for e in elems.iter().flatten() {
                    add(&e.0, out);
                }
            }
            AssignTarget::DestructuringArrayRest { elements, rest } => {
                for e in elements.iter().flatten() {
                    add(&e.0, out);
                }
                add(rest, out);
            }
            AssignTarget::DestructuringObject(props) => {
                for p in props {
                    add(&p.1, out);
                }
            }
            AssignTarget::DestructuringObjectRest { properties, rest } => {
                for p in properties {
                    add(&p.1, out);
                }
                add(rest, out);
            }
            _ => {}
        }
    }
    add(target, &mut out);
    out
}
