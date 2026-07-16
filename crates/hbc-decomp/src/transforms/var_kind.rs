// Infer `const` vs `let` from reassignment analysis.
// Hermes bytecode does not distinguish them, this is a post-hoc readability pass.

use crate::ir::{map_nested_bodies_mut, AssignTarget, Statement, VarKind};
use std::collections::HashSet;

/// Promote `let` bindings that are never reassigned to `const`.
pub fn promote_const_bindings(stmts: &mut [Statement]) {
    let mut assigned = HashSet::new();
    collect_assignments(stmts, &mut assigned);
    apply_const(stmts, &assigned);
}

fn collect_assignments(stmts: &[Statement], assigned: &mut HashSet<String>) {
    for stmt in stmts {
        match stmt {
            Statement::Assign {
                target: AssignTarget::Variable(name),
                ..
            } => {
                assigned.insert(name.clone());
            }
            Statement::Assign { target, .. } => collect_assign_target(target, assigned),
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                collect_assignments(then_body, assigned);
                collect_assignments(else_body, assigned);
            }
            Statement::While { body, .. }
            | Statement::DoWhile { body, .. }
            | Statement::ForIn { body, .. }
            | Statement::ForOf { body, .. }
            | Statement::Block(body) => collect_assignments(body, assigned),
            Statement::For {
                init, update, body, ..
            } => {
                if let Some(i) = init {
                    collect_assignments(std::slice::from_ref(i.as_ref()), assigned);
                }
                if let Some(u) = update {
                    collect_assignments(std::slice::from_ref(u.as_ref()), assigned);
                }
                collect_assignments(body, assigned);
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                finally_body,
                ..
            } => {
                collect_assignments(try_body, assigned);
                collect_assignments(catch_body, assigned);
                collect_assignments(finally_body, assigned);
            }
            Statement::Switch { cases, default, .. } => {
                for (_, body) in cases {
                    collect_assignments(body, assigned);
                }
                if let Some(d) = default {
                    collect_assignments(d, assigned);
                }
            }
            Statement::Class {
                constructor,
                methods,
                ..
            } => {
                if let Some(c) = constructor {
                    collect_assignments(std::slice::from_ref(c.as_ref()), assigned);
                }
                for m in methods {
                    // method bodies are expressions or nested, skip deep for V1
                    let _ = m;
                }
            }
            _ => {}
        }
    }
}

fn collect_assign_target(target: &AssignTarget, assigned: &mut HashSet<String>) {
    match target {
        AssignTarget::Variable(n) => {
            assigned.insert(n.clone());
        }
        AssignTarget::DestructuringObject(props) => {
            for (_, t, _) in props {
                collect_assign_target(t, assigned);
            }
        }
        AssignTarget::DestructuringArray(elems) => {
            for e in elems.iter().flatten() {
                collect_assign_target(&e.0, assigned);
            }
        }
        AssignTarget::Rest(inner) => collect_assign_target(inner, assigned),
        _ => {}
    }
}

fn apply_const(stmts: &mut [Statement], assigned: &HashSet<String>) {
    for stmt in stmts.iter_mut() {
        match stmt {
            Statement::Let { name, kind, .. } => {
                if *kind == VarKind::Let && !assigned.contains(name) {
                    *kind = VarKind::Const;
                }
            }
            other => {
                map_nested_bodies_mut(other, |mut body| {
                    apply_const(&mut body, assigned);
                    body
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Constant, Expression, Value};

    #[test]
    fn promotes_unassigned_let_to_const() {
        let mut stmts = vec![
            Statement::Let {
                name: "x".into(),
                value: Expression::Value(Value::Constant(Constant::Integer(1))),
                kind: VarKind::Let,
            },
            Statement::Let {
                name: "y".into(),
                value: Expression::Value(Value::Constant(Constant::Integer(2))),
                kind: VarKind::Let,
            },
            Statement::Assign {
                target: AssignTarget::Variable("y".into()),
                value: Expression::Value(Value::Constant(Constant::Integer(3))),
            },
        ];
        promote_const_bindings(&mut stmts);
        match &stmts[0] {
            Statement::Let { kind, .. } => assert_eq!(*kind, VarKind::Const),
            _ => panic!(),
        }
        match &stmts[1] {
            Statement::Let { kind, .. } => assert_eq!(*kind, VarKind::Let),
            _ => panic!(),
        }
    }
}
