// Fold chain assignments: `r0 = x; y = r0` -> `y = x`

use crate::ir::{Statement, Expression, AssignTarget, Value};

// Fold chain assignments. Only when r0 is used exactly once immediately after.
pub(super) fn fold_chain_assignments(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut result = Vec::new();
    let mut iter = stmts.into_iter().peekable();

    while let Some(stmt) = iter.next() {
        match &stmt {
            Statement::Assign {
                target: AssignTarget::Register(r),
                value,
            } if !value.has_side_effects() => {
                // Check if next statement uses this register as its value
                if let Some(Statement::Assign {
                    target: next_target,
                    value: Expression::Value(Value::Register(r2)),
                }) = iter.peek()
                {
                    if r == r2 && !matches!(next_target, AssignTarget::Register(_)) {
                        // Fold: skip current, modify next
                        let Some(next) = iter.next() else { continue };
                        if let Statement::Assign { target: t, .. } = next {
                            result.push(Statement::Assign {
                                target: t,
                                value: value.clone(),
                            });
                            continue;
                        }
                    }
                }
                result.push(stmt);
            }
            Statement::If { condition, then_body, else_body } => {
                result.push(Statement::If {
                    condition: condition.clone(),
                    then_body: fold_chain_assignments(then_body.clone()),
                    else_body: fold_chain_assignments(else_body.clone()),
                });
            }
            Statement::While { condition, body } => {
                result.push(Statement::While {
                    condition: condition.clone(),
                    body: fold_chain_assignments(body.clone()),
                });
            }
            Statement::Block(inner) => {
                result.push(Statement::Block(fold_chain_assignments(inner.clone())));
            }
            _ => result.push(stmt),
        }
    }

    result
}
