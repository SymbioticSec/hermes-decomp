use crate::ir::{AssignTarget, Binding, Expression, Statement};

// Collect destructuring-pattern target names across the whole function.
pub(super) fn collect_pattern_names(stmts: &[Statement], out: &mut Vec<String>) {
    for stmt in stmts {
        let target = match stmt {
            Statement::Assign { target, .. } => Some(target),
            Statement::Expr(Expression::Assignment { target, .. }) => Some(target.as_ref()),
            _ => None,
        };
        if let Some(target) = target {
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
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                collect_pattern_names(then_body, out);
                collect_pattern_names(else_body, out);
            }
            Statement::While { body, .. }
            | Statement::DoWhile { body, .. }
            | Statement::For { body, .. }
            | Statement::ForIn { body, .. }
            | Statement::ForOf { body, .. } => collect_pattern_names(body, out),
            Statement::Block(inner) => collect_pattern_names(inner, out),
            Statement::TryCatch {
                try_body,
                catch_body,
                finally_body,
                ..
            } => {
                collect_pattern_names(try_body, out);
                collect_pattern_names(catch_body, out);
                collect_pattern_names(finally_body, out);
            }
            // Case patterns are declared at the start of that case, not once
            // for the whole function. One hoist would make every case share
            // the same binding.
            Statement::Switch { .. } => {}
            _ => {}
        }
    }
}

// Variable names bound by a destructuring assignment target.
pub(super) fn destructuring_target_names(target: &AssignTarget) -> Vec<String> {
    let mut out = Vec::new();
    fn add(t: &AssignTarget, out: &mut Vec<String>) {
        match t {
            AssignTarget::Binding(Binding::Variable(n)) => out.push(n.clone()),
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
