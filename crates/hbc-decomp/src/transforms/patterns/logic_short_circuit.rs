use crate::ir::{AssignTarget, BinaryOp, Expression, Statement, Value, MutVisitor};

// Detect and fold short-circuit logic operators (`&&`, `||`, `??`).
//
// Converts CFG-generated jump patterns into short-circuit logic operators:
//
// For `a || b`:
// ```javascript
// r1 = a;
// if (!r1) {
//     r1 = b;
// }
// ```
//
// For `a && b`:
// ```javascript
// r1 = a;
// if (r1) { // condition may not be exactly `r1` if simplify is run before
//     r1 = b;
// }
// ```
//
// For `a ?? b`:
// ```javascript
// r1 = a;
// if (r1 == null) {
//     r1 = b;
// }
// ```
pub fn detect_short_circuit_logic(mut stmts: Vec<Statement>) -> Vec<Statement> {
    let mut visitor = ShortCircuitVisitor;
    visitor.visit_statement_list(&mut stmts);
    stmts
}

struct ShortCircuitVisitor;

impl MutVisitor for ShortCircuitVisitor {
    fn visit_statement_list(&mut self, stmts: &mut Vec<Statement>) {
        // Recurse first to fold inner blocks
        self.walk_statement_list(stmts);

        let mut i = 0;
        while i < stmts.len() {
            if i + 1 >= stmts.len() {
                break;
            }

            // Look for `r1 = a; if (cond) { r1 = b; }` pattern
            let match_result = if let Statement::Assign { target: t1, value: _a_expr } = &stmts[i] {
                if let Statement::If { condition, then_body, else_body } = &stmts[i + 1] {
                    // if-statement must have an empty else body, and exactly one assignment in then_body
                    if else_body.is_empty() && then_body.len() == 1 {
                        if let Statement::Assign { target: t2, value: b_expr } = &then_body[0] {
                            // Target must be exactly the same
                            if targets_equal(t1, t2) {
                                // Now, analyze the condition relative to the target to determine the operator
                                determine_short_circuit_op(t1, condition).map(|op| (t1.clone(), op, b_expr.clone()))
                            } else { None }
                        } else { None }
                    } else { None }
                } else { None }
            } else { None };

            if let Some((target, op, b_expr)) = match_result {
                // We have a match! Fold them!
                let a_expr = match &mut stmts[i] {
                    Statement::Assign { value, .. } => std::mem::replace(value, Expression::constant(crate::ir::Constant::Undefined)),
                    _ => unreachable!(),
                };

                stmts[i] = Statement::Assign {
                    target,
                    value: Expression::binary(op, a_expr, b_expr),
                };

                // Remove the following if statement
                stmts.remove(i + 1);
            } else {
                i += 1;
            }
        }
    }
}

fn targets_equal(t1: &AssignTarget, t2: &AssignTarget) -> bool {
    t1 == t2
}

fn determine_short_circuit_op(target: &AssignTarget, condition: &Expression) -> Option<BinaryOp> {
    // If target is a register, we expect the condition to use it
    let target_reg = match target {
        AssignTarget::Register(r) => *r,
        _ => return None, // Complex to determine safely for non-registers right now
    };

    match condition {
        // `if (r1)` -> jump if truthy. This means we execute the `then` block if `r1` is TRUE.
        // The `then` block assigns `r1 = b`.
        // So `r1 = a; if (r1) r1 = b;` corresponds to `a && b`.
        Expression::Value(Value::Register(r)) if *r == target_reg => {
            Some(BinaryOp::LogicalAnd)
        }

        // `if (!r1)` -> jump if falsy. We execute `then` block if `r1` is FALSE.
        // `r1 = a; if (!r1) r1 = b;` corresponds to `a || b`.
        Expression::Unary { op: crate::ir::UnaryOp::Not, operand } => {
            if let Expression::Value(Value::Register(r)) = &**operand {
                if *r == target_reg {
                    return Some(BinaryOp::LogicalOr);
                }
            }
            None
        }

        // `if (r1 == null)` -> nullish coalesce. We execute `then` block if `r1` is nullish (Hermes transpiles ?? to `!= null` jump, so falling through means it was `== null`).
        Expression::Binary { op: BinaryOp::Eq, left, right } | Expression::Binary { op: BinaryOp::StrictEq, left, right } => {
            if is_null_or_undefined_check(left, right, target_reg) {
                Some(BinaryOp::NullishCoalesce)
            } else {
                None
            }
        }

        _ => None,
    }
}

fn is_null_or_undefined_check(left: &Expression, right: &Expression, target_reg: u32) -> bool {
    // Check if one side is `target_reg` and the other side is null/undefined
    if let Expression::Value(Value::Register(r)) = left {
        if *r == target_reg && is_null_or_undefined(right) {
            return true;
        }
    }
    if let Expression::Value(Value::Register(r)) = right {
        if *r == target_reg && is_null_or_undefined(left) {
            return true;
        }
    }
    false
}

fn is_null_or_undefined(expr: &Expression) -> bool {
    super::utils::is_null_or_undefined(expr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Constant;

    #[test]
    fn test_logical_or() {
        let stmts = vec![
            Statement::assign_reg(1, Expression::constant(Constant::Integer(1))),
            Statement::If {
                condition: Expression::unary(crate::ir::UnaryOp::Not, Expression::register(1)),
                then_body: vec![Statement::assign_reg(1, Expression::constant(Constant::Integer(2)))],
                else_body: vec![],
            }
        ];

        let result = detect_short_circuit_logic(stmts);
        
        assert_eq!(result.len(), 1);
        if let Statement::Assign { target: AssignTarget::Register(1), value: Expression::Binary { op: BinaryOp::LogicalOr, .. } } = &result[0] {
            // Success
        } else {
            panic!("Failed to fold LogicalOr");
        }
    }

    #[test]
    fn test_logical_and() {
        let stmts = vec![
            Statement::assign_reg(1, Expression::constant(Constant::Integer(1))),
            Statement::If {
                condition: Expression::register(1),
                then_body: vec![Statement::assign_reg(1, Expression::constant(Constant::Integer(2)))],
                else_body: vec![],
            }
        ];

        let result = detect_short_circuit_logic(stmts);
        
        assert_eq!(result.len(), 1);
        if let Statement::Assign { target: AssignTarget::Register(1), value: Expression::Binary { op: BinaryOp::LogicalAnd, .. } } = &result[0] {
            // Success
        } else {
            panic!("Failed to fold LogicalAnd");
        }
    }
}
