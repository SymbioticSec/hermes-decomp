use crate::ir::{map_nested_bodies, Statement, Expression, AssignTarget, Value};

pub(super) fn remove_redundant_assignments(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts
        .into_iter()
        .filter(|stmt| {
            if let Statement::Assign {
                target: AssignTarget::Register(r),
                value: Expression::Value(Value::Register(r2)),
            } = stmt
            {
                return r != r2;
            }
            true
        })
        .map(|stmt| map_nested_bodies(stmt, remove_redundant_assignments))
        .collect()
}
