use crate::ir::{Statement, Expression, Value, BinaryOp, AssignTarget};

// Detect for loop patterns from while loops.
pub fn detect_for_loops(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut result = Vec::new();
    let mut iter = stmts.into_iter().peekable();

    while let Some(stmt) = iter.next() {
        match stmt {
            // Check if this assignment might be a for-loop init followed by while
            Statement::Assign { target, value } => {
                if let Some(Statement::While { condition, body }) = iter.peek() {
                    // Check if this is a for-loop pattern
                    if let Some((update, new_body)) = extract_for_loop_update(body) {
                        // Check if the while condition uses the same variable
                        if uses_variable(condition, &target) {
                            let Some(while_stmt) = iter.next() else { continue };
                            if let Statement::While { condition, body: _ } = while_stmt {
                                result.push(Statement::For {
                                    init: Some(Box::new(Statement::Assign {
                                        target,
                                        value,
                                    })),
                                    condition: Some(condition),
                                    update: Some(Box::new(update)),
                                    body: detect_for_loops(new_body),
                                });
                                continue;
                            }
                        }
                    }
                }
                result.push(Statement::Assign { target, value });
            }
            Statement::While { condition, body } => {
                // Check for for-loop pattern without preceding init
                if let Some((update, new_body)) = extract_for_loop_update(&body) {
                    result.push(Statement::For {
                        init: None,
                        condition: Some(condition),
                        update: Some(Box::new(update)),
                        body: detect_for_loops(new_body),
                    });
                } else {
                    result.push(Statement::While {
                        condition,
                        body: detect_for_loops(body),
                    });
                }
            }
            Statement::If { condition, then_body, else_body } => {
                result.push(Statement::If {
                    condition,
                    then_body: detect_for_loops(then_body),
                    else_body: detect_for_loops(else_body),
                });
            }
            Statement::Block(inner) => {
                result.push(Statement::Block(detect_for_loops(inner)));
            }
            other => result.push(other),
        }
    }
    result
}

// Extract a for-loop update statement from the end of a while body.
fn extract_for_loop_update(body: &[Statement]) -> Option<(Statement, Vec<Statement>)> {
    if body.is_empty() {
        return None;
    }

    let last = body.last()?;

    // Look for increment patterns: i = i + 1, i++, ++i
    match last {
        Statement::Assign { target, value: Expression::Binary { op, left, right } }
            if matches!(op, BinaryOp::Add | BinaryOp::Sub)
                && (is_same_target(target, left) || is_same_target(target, right)) =>
        {
            let new_body = body[..body.len() - 1].to_vec();
            Some((last.clone(), new_body))
        }
        Statement::Expr(Expression::Assignment { .. }) => {
            let new_body = body[..body.len() - 1].to_vec();
            Some((last.clone(), new_body))
        }
        _ => None,
    }
}

// Check if an expression uses a given assignment target.
fn uses_variable(expr: &Expression, target: &AssignTarget) -> bool {
    match (expr, target) {
        (Expression::Value(Value::Register(r1)), AssignTarget::Register(r2)) => r1 == r2,
        (Expression::Value(Value::Variable(v1)), AssignTarget::Variable(v2)) => v1 == v2,
        (Expression::Binary { left, right, .. }, _) => {
            uses_variable(left, target) || uses_variable(right, target)
        }
        _ => false,
    }
}

// Check if an expression is the same as an assignment target.
fn is_same_target(target: &AssignTarget, expr: &Expression) -> bool {
    match (target, expr) {
        (AssignTarget::Register(r1), Expression::Value(Value::Register(r2))) => r1 == r2,
        (AssignTarget::Variable(v1), Expression::Value(Value::Variable(v2))) => v1 == v2,
        _ => false,
    }
}
