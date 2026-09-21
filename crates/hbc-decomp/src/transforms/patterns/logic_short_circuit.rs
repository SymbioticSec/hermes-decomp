use crate::ir::{Binding, AssignTarget, BinaryOp, Expression, Statement, Value, MutVisitor};

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

            // The first statement can be an assignment or a declaration. After
            // naming, a folded value is introduced as `let t = a;`, and skipping
            // that shape left the whole pattern standing.
            let first_target = match &stmts[i] {
                Statement::Assign { target, .. } => Some(target.clone()),
                Statement::Let { name, .. } => Some(AssignTarget::Binding(Binding::Variable(name.clone()))),
                _ => None,
            };
            let match_result = if let Some(t1) = first_target.as_ref() {
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
                let (a_expr, declared) = match &mut stmts[i] {
                    Statement::Assign { value, .. } => (
                        std::mem::replace(value, Expression::constant(crate::ir::Constant::Undefined)),
                        None,
                    ),
                    Statement::Let { value, name, kind } => (
                        std::mem::replace(value, Expression::constant(crate::ir::Constant::Undefined)),
                        Some((name.clone(), *kind)),
                    ),
                    _ => unreachable!(),
                };

                let folded = Expression::binary(op, a_expr, b_expr);
                stmts[i] = match declared {
                    // A declaration stays a declaration, otherwise the binding it
                    // introduced would vanish and every later use dangle.
                    Some((name, kind)) => Statement::Let { name, value: folded, kind },
                    None => Statement::Assign { target, value: folded },
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

// Whether the expression reads exactly the binding the statement writes. The
// binding is a register before naming and a named variable after it, and the
// pass used to accept only the first, so nothing folded once names were in.
fn reads_target(expr: &Expression, target: &AssignTarget) -> bool {
    match (expr, target) {
        (Expression::Value(Value::Binding(Binding::Register(r))), AssignTarget::Binding(Binding::Register(t))) => r == t,
        (Expression::Value(Value::Binding(Binding::Variable(n))), AssignTarget::Binding(Binding::Variable(t))) => n == t,
        _ => false,
    }
}

fn determine_short_circuit_op(target: &AssignTarget, condition: &Expression) -> Option<BinaryOp> {
    if !matches!(target, AssignTarget::Binding(Binding::Register(_)) | AssignTarget::Binding(Binding::Variable(_))) {
        return None;
    }

    match condition {
        // `if (r1)` -> jump if truthy. This means we execute the `then` block if `r1` is TRUE.
        // The `then` block assigns `r1 = b`.
        // So `r1 = a; if (r1) r1 = b;` corresponds to `a && b`.
        cond if reads_target(cond, target) => Some(BinaryOp::LogicalAnd),

        // `if (!r1)` -> jump if falsy. We execute `then` block if `r1` is FALSE.
        // `r1 = a; if (!r1) r1 = b;` corresponds to `a || b`.
        Expression::Unary { op: crate::ir::UnaryOp::Not, operand } => {
            if reads_target(operand, target) {
                return Some(BinaryOp::LogicalOr);
            }
            None
        }

        // `if (r1 == null)` -> nullish coalesce. We execute `then` block if `r1` is nullish (Hermes transpiles ?? to `!= null` jump, so falling through means it was `== null`).
        Expression::Binary { op: BinaryOp::Eq, left, right } | Expression::Binary { op: BinaryOp::StrictEq, left, right } => {
            if is_null_or_undefined_check(left, right, target) {
                Some(BinaryOp::NullishCoalesce)
            } else {
                None
            }
        }

        _ => None,
    }
}

fn is_null_or_undefined_check(
    left: &Expression,
    right: &Expression,
    target: &AssignTarget,
) -> bool {
    (reads_target(left, target) && is_null_or_undefined(right))
        || (reads_target(right, target) && is_null_or_undefined(left))
}

fn is_null_or_undefined(expr: &Expression) -> bool {
    super::utils::is_null_or_undefined(expr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Constant;

    fn named_assign(name: &str, value: Expression) -> Statement {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(name.to_string())),
            value,
        }
    }

    fn var(name: &str) -> Expression {
        Expression::Value(Value::Binding(Binding::Variable(name.to_string())))
    }

    #[test]
    fn a_named_binding_folds_just_like_a_register() {
        // Register naming runs before this pass sees a body a second time, so the
        // pattern arrives spelled in names. Accepting only registers left every one
        // of those standing.
        let stmts = vec![
            named_assign("env", var("source")),
            Statement::If {
                condition: Expression::unary(crate::ir::UnaryOp::Not, var("env")),
                then_body: vec![named_assign("env", Expression::constant(Constant::Integer(2)))],
                else_body: vec![],
            },
        ];
        let out = detect_short_circuit_logic(stmts);
        assert_eq!(out.len(), 1);
        match &out[0] {
            Statement::Assign { value: Expression::Binary { op, .. }, .. } => {
                assert_eq!(*op, BinaryOp::LogicalOr)
            }
            other => panic!("expected a folded or, got {other:?}"),
        }
    }

    #[test]
    fn a_declaration_stays_a_declaration_once_folded() {
        // Folding must not turn `let x = a; if (!x) { x = b; }` into a bare
        // assignment, or the binding disappears and every later use dangles.
        let stmts = vec![
            Statement::Let {
                name: "env".to_string(),
                value: var("source"),
                kind: crate::ir::VarKind::Let,
            },
            Statement::If {
                condition: Expression::unary(crate::ir::UnaryOp::Not, var("env")),
                then_body: vec![named_assign("env", Expression::constant(Constant::Integer(2)))],
                else_body: vec![],
            },
        ];
        let out = detect_short_circuit_logic(stmts);
        assert_eq!(out.len(), 1);
        match &out[0] {
            Statement::Let { name, value: Expression::Binary { op, .. }, .. } => {
                assert_eq!(name, "env");
                assert_eq!(*op, BinaryOp::LogicalOr);
            }
            other => panic!("expected a folded let, got {other:?}"),
        }
    }

    #[test]
    fn a_different_binding_in_the_body_is_left_alone() {
        // The body must write the same binding the head does, otherwise the two
        // statements are unrelated and folding them would invent a value.
        let stmts = vec![
            named_assign("env", var("source")),
            Statement::If {
                condition: Expression::unary(crate::ir::UnaryOp::Not, var("env")),
                then_body: vec![named_assign("other", Expression::constant(Constant::Integer(2)))],
                else_body: vec![],
            },
        ];
        let out = detect_short_circuit_logic(stmts.clone());
        assert_eq!(out.len(), 2, "unrelated statements must not fold");
    }

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
        if let Statement::Assign { target: AssignTarget::Binding(Binding::Register(1)), value: Expression::Binary { op: BinaryOp::LogicalOr, .. } } = &result[0] {
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
        if let Statement::Assign { target: AssignTarget::Binding(Binding::Register(1)), value: Expression::Binary { op: BinaryOp::LogicalAnd, .. } } = &result[0] {
            // Success
        } else {
            panic!("Failed to fold LogicalAnd");
        }
    }
}
