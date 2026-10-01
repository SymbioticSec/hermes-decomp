use crate::ir::{map_nested_bodies, AssignTarget, Binding, Expression, Statement, Value};

pub(super) fn remove_redundant_assignments(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts
        .into_iter()
        .filter(|stmt| {
            if let Statement::Assign {
                target: AssignTarget::Binding(Binding::Register(r)),
                value: Expression::Value(Value::Binding(Binding::Register(r2))),
            } = stmt
            {
                return r != r2;
            }
            true
        })
        .map(|stmt| map_nested_bodies(stmt, remove_redundant_assignments))
        .collect()
}
