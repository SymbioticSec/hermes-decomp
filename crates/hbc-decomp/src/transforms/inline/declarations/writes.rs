use super::patterns::destructuring_target_names;
use crate::ir::{AssignTarget, Statement};
use std::collections::BTreeMap;

pub(super) fn count_writes(
    stmts: &[Statement],
    writes: &mut BTreeMap<String, usize>,
    let_declared: &mut std::collections::HashSet<String>,
) {
    for stmt in stmts {
        count_writes_stmt(stmt, writes, let_declared);
    }
}

fn count_writes_stmt(
    stmt: &Statement,
    writes: &mut BTreeMap<String, usize>,
    let_declared: &mut std::collections::HashSet<String>,
) {
    match stmt {
        Statement::Assign { target: AssignTarget::Variable(name), .. } => {
            *writes.entry(name.clone()).or_insert(0) += 1;
        }
        Statement::Assign { target: AssignTarget::Register(r), .. } => {
            *writes.entry(format!("r{r}")).or_insert(0) += 1;
        }
        // A destructuring assign (`let [a, b] = e`) is rendered as its own `let`
        // declaration; record its bound names so a later `a = ...` is a plain
        // reassignment, not a duplicate declaration.
        Statement::Assign { target, .. }
            if matches!(
                target,
                AssignTarget::DestructuringArray(_)
                    | AssignTarget::DestructuringArrayRest { .. }
                    | AssignTarget::DestructuringObject(_)
                    | AssignTarget::DestructuringObjectRest { .. }
            ) =>
        {
            for name in destructuring_target_names(target) {
                let_declared.insert(name);
            }
        }
        Statement::Let { name, .. } => {
            let_declared.insert(name.clone());
        }
        Statement::If { condition: _, then_body, else_body } => {
            count_writes(then_body, writes, let_declared);
            count_writes(else_body, writes, let_declared);
        }
        Statement::While { body, .. } | Statement::DoWhile { body, .. }
        | Statement::For { body, .. } | Statement::ForIn { body, .. }
        | Statement::ForOf { body, .. } => {
            // Variables assigned inside loops are always multi-write
            let mut inner_writes: BTreeMap<String, usize> = BTreeMap::new();
            let mut inner_lets = std::collections::HashSet::new();
            count_writes(body, &mut inner_writes, &mut inner_lets);
            for (name, count) in inner_writes {
                // Treat loop body assignments as at least 2 writes (since loops repeat)
                *writes.entry(name).or_insert(0) += count.max(2);
            }
            let_declared.extend(inner_lets);
        }
        Statement::Block(inner) => count_writes(inner, writes, let_declared),
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
            count_writes(try_body, writes, let_declared);
            count_writes(catch_body, writes, let_declared);
            count_writes(finally_body, writes, let_declared);
        }
        Statement::Switch { cases, default, .. } => {
            for (_, body) in cases {
                count_writes(body, writes, let_declared);
            }
            if let Some(d) = default {
                count_writes(d, writes, let_declared);
            }
        }
        _ => {}
    }
}
