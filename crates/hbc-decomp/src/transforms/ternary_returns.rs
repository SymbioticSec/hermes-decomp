// Ternary optimization for return statements.
//
// Transforms patterns like:
//   if (c) { return a; } else { return b; }
// Into:
//   return c ? a : b;
//
// Also handles:
//   if (c) { return a; }
//   return b;

use crate::ir::{Expression, Statement};

pub fn optimize_ternary_returns(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut result = Vec::with_capacity(stmts.len());
    let mut i = 0;

    while i < stmts.len() {
        // Pattern 1: if (c) { return a; } else { return b; }
        if let Some(ternary) = try_if_else_return(&stmts[i]) {
            result.push(ternary);
            i += 1;
            continue;
        }

        // Pattern 2: if (c) { return a; } return b;
        if i + 1 < stmts.len() {
            if let Some(ternary) = try_if_return_fallthrough(&stmts[i], &stmts[i + 1]) {
                result.push(ternary);
                i += 2;
                continue;
            }
        }

        result.push(process_nested(stmts[i].clone()));
        i += 1;
    }

    result
}

// Try to convert `if (c) { return a; } else { return b; }` to `return c ? a : b;`
//
// Heuristic:
// We check `is_reasonable_complexity` to avoid creating unreadable nested ternaries.
// If the expressions are too deep (e.g. nested calls), we keep the `if/else` structure for readability.
fn try_if_else_return(stmt: &Statement) -> Option<Statement> {
    if let Statement::If {
        condition,
        then_body,
        else_body,
    } = stmt
    {
        // Both branches must have exactly one return statement
        if then_body.len() == 1 && else_body.len() == 1 {
            if let (Statement::Return(Some(then_val)), Statement::Return(Some(else_val))) =
                (&then_body[0], &else_body[0])
            {
                // Check complexity - don't create overly complex ternaries
                if is_reasonable_complexity(condition)
                    && is_reasonable_complexity(then_val)
                    && is_reasonable_complexity(else_val)
                {
                    return Some(Statement::Return(Some(Expression::Conditional {
                        condition: Box::new(condition.clone()),
                        then_expr: Box::new(then_val.clone()),
                        else_expr: Box::new(else_val.clone()),
                    })));
                }
            }
        }

        // Handle: if (c) { return a; } else { return; } → return c ? a : undefined;
        if then_body.len() == 1 && else_body.len() == 1 {
            if let (Statement::Return(Some(then_val)), Statement::Return(None)) =
                (&then_body[0], &else_body[0])
            {
                if is_reasonable_complexity(condition) && is_reasonable_complexity(then_val) {
                    return Some(Statement::Return(Some(Expression::Conditional {
                        condition: Box::new(condition.clone()),
                        then_expr: Box::new(then_val.clone()),
                        else_expr: Box::new(Expression::Value(crate::ir::Value::Constant(
                            crate::ir::Constant::Undefined,
                        ))),
                    })));
                }
            }
        }
    }

    None
}

// Try to convert `if (c) { return a; } return b;` to `return c ? a : b;`
fn try_if_return_fallthrough(if_stmt: &Statement, next_stmt: &Statement) -> Option<Statement> {
    if let Statement::If {
        condition,
        then_body,
        else_body,
    } = if_stmt
    {
        // Else body must be empty
        if !else_body.is_empty() {
            return None;
        }

        // Then body must have exactly one return
        if then_body.len() != 1 {
            return None;
        }

        if let Statement::Return(Some(then_val)) = &then_body[0] {
            // Next statement must be a return
            if let Statement::Return(Some(else_val)) = next_stmt {
                if is_reasonable_complexity(condition)
                    && is_reasonable_complexity(then_val)
                    && is_reasonable_complexity(else_val)
                {
                    return Some(Statement::Return(Some(Expression::Conditional {
                        condition: Box::new(condition.clone()),
                        then_expr: Box::new(then_val.clone()),
                        else_expr: Box::new(else_val.clone()),
                    })));
                }
            }

            // Handle: if (c) { return a; } return;
            if let Statement::Return(None) = next_stmt {
                if is_reasonable_complexity(condition) && is_reasonable_complexity(then_val) {
                    return Some(Statement::Return(Some(Expression::Conditional {
                        condition: Box::new(condition.clone()),
                        then_expr: Box::new(then_val.clone()),
                        else_expr: Box::new(Expression::Value(crate::ir::Value::Constant(
                            crate::ir::Constant::Undefined,
                        ))),
                    })));
                }
            }
        }
    }

    None
}

// Check if an expression is simple enough for a ternary.
fn is_reasonable_complexity(expr: &Expression) -> bool {
    complexity(expr) <= 5
}

// Calculate expression complexity score.
fn complexity(expr: &Expression) -> u32 {
    match expr {
        Expression::Value(_) => 1,
        Expression::Member { object, .. } => 1 + complexity(object),
        Expression::Call { callee, arguments } => {
            2 + complexity(callee) + arguments.iter().map(complexity).sum::<u32>()
        }
        Expression::New { callee, arguments } => {
            2 + complexity(callee) + arguments.iter().map(complexity).sum::<u32>()
        }
        Expression::Binary { left, right, .. } => 1 + complexity(left) + complexity(right),
        Expression::Unary { operand, .. } => 1 + complexity(operand),
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => 2 + complexity(condition) + complexity(then_expr) + complexity(else_expr),
        Expression::Array { elements } => {
            1 + elements.iter().flatten().map(complexity).sum::<u32>()
        }
        Expression::Object { properties } => {
            1 + properties.iter().map(|p| complexity(&p.value)).sum::<u32>()
        }
        _ => 2,
    }
}

fn process_nested(stmt: Statement) -> Statement {
    crate::ir::map_nested_bodies(stmt, optimize_ternary_returns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Constant, Value};

    #[test]
    fn test_if_else_return_to_ternary() {
        let stmts = vec![Statement::If {
            condition: Expression::Value(Value::Variable("x".to_string())),
            then_body: vec![Statement::Return(Some(Expression::constant(
                Constant::Integer(1),
            )))],
            else_body: vec![Statement::Return(Some(Expression::constant(
                Constant::Integer(2),
            )))],
        }];

        let result = optimize_ternary_returns(stmts);

        assert_eq!(result.len(), 1);
        if let Statement::Return(Some(Expression::Conditional { .. })) = &result[0] {
            // Success
        } else {
            panic!("Expected ternary return, got: {:?}", result[0]);
        }
    }

    #[test]
    fn test_if_return_fallthrough_to_ternary() {
        let stmts = vec![
            Statement::If {
                condition: Expression::Value(Value::Variable("x".to_string())),
                then_body: vec![Statement::Return(Some(Expression::constant(
                    Constant::Integer(1),
                )))],
                else_body: vec![],
            },
            Statement::Return(Some(Expression::constant(Constant::Integer(2)))),
        ];

        let result = optimize_ternary_returns(stmts);

        assert_eq!(result.len(), 1);
        if let Statement::Return(Some(Expression::Conditional { .. })) = &result[0] {
            // Success
        } else {
            panic!("Expected ternary return, got: {:?}", result);
        }
    }

    #[test]
    fn test_complex_not_converted() {
        // Very complex expressions should not be converted to ternary
        let complex_expr = Expression::Call {
            callee: Box::new(Expression::Call {
                callee: Box::new(Expression::Value(Value::Variable("f".to_string()))),
                arguments: vec![Expression::Call {
                    callee: Box::new(Expression::Value(Value::Variable("g".to_string()))),
                    arguments: vec![Expression::constant(Constant::Integer(1))],
                }],
            }),
            arguments: vec![],
        };

        let stmts = vec![Statement::If {
            condition: Expression::Value(Value::Variable("x".to_string())),
            then_body: vec![Statement::Return(Some(complex_expr.clone()))],
            else_body: vec![Statement::Return(Some(complex_expr))],
        }];

        let result = optimize_ternary_returns(stmts);

        // Should remain as if-else due to complexity
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Statement::If { .. }));
    }
}
