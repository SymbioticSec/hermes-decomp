// Detect ternary patterns in if/else assignments.

use crate::ir::{Statement, Expression, AssignTarget};

// Detect ternary patterns: if (c) { r = a } else { r = b } -> r = c ? a : b
pub(super) fn detect_ternaries(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts.into_iter().map(detect_ternary).collect()
}

fn detect_ternary(stmt: Statement) -> Statement {
    match stmt {
        Statement::If { condition, then_body, else_body } => {
            // Check if both branches are single assignments to same target
            if let (Some(then_assign), Some(else_assign)) = (
                get_single_assignment(&then_body),
                get_single_assignment(&else_body),
            ) {
                if then_assign.0 == else_assign.0 {
                    return Statement::Assign {
                        target: then_assign.0,
                        value: Expression::Conditional {
                            condition: Box::new(condition),
                            then_expr: Box::new(then_assign.1),
                            else_expr: Box::new(else_assign.1),
                        },
                    };
                }
            }

            // Recurse into branches
            Statement::If {
                condition,
                then_body: detect_ternaries(then_body),
                else_body: detect_ternaries(else_body),
            }
        }
        Statement::While { condition, body } => Statement::While {
            condition,
            body: detect_ternaries(body),
        },
        Statement::Block(inner) => Statement::Block(detect_ternaries(inner)),
        _ => stmt,
    }
}

fn get_single_assignment(stmts: &[Statement]) -> Option<(AssignTarget, Expression)> {
    if stmts.len() != 1 {
        return None;
    }
    match &stmts[0] {
        Statement::Assign { target, value } => Some((target.clone(), value.clone())),
        _ => None,
    }
}

