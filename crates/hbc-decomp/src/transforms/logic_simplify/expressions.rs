use super::utils::{
    exprs_equal, is_boolean_expr, is_falsy, is_simple_value, is_truthy, negate_expr,
};
use crate::ir::{BinaryOp, Constant, Expression, UnaryOp, Value};

pub fn simplify_expr(expr: Expression) -> Expression {
    match expr {
        Expression::Unary {
            op: UnaryOp::Not,
            operand,
        } => simplify_unary_not(simplify_expr(*operand)),

        Expression::Binary { op, left, right } => {
            simplify_binary(op, simplify_expr(*left), simplify_expr(*right))
        }

        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => simplify_conditional(
            simplify_expr(*condition),
            simplify_expr(*then_expr),
            simplify_expr(*else_expr),
        ),

        // Recursively simplify sub-expressions
        Expression::Call { callee, arguments } => Expression::Call {
            callee: Box::new(simplify_expr(*callee)),
            arguments: arguments.into_iter().map(simplify_expr).collect(),
        },
        Expression::New { callee, arguments } => Expression::New {
            callee: Box::new(simplify_expr(*callee)),
            arguments: arguments.into_iter().map(simplify_expr).collect(),
        },
        Expression::Member {
            object,
            property,
            optional,
        } => Expression::Member {
            object: Box::new(simplify_expr(*object)),
            property,
            optional,
        },
        Expression::Array { elements } => Expression::Array {
            elements: elements.into_iter().map(|e| e.map(simplify_expr)).collect(),
        },
        Expression::Object { properties } => Expression::Object {
            properties: properties
                .into_iter()
                .map(|p| crate::ir::ObjectProperty {
                    key: p.key,
                    value: simplify_expr(p.value),
                })
                .collect(),
        },
        Expression::Assignment { target, value } => Expression::Assignment {
            target: Box::new(simplify_expr(*target)),
            value: Box::new(simplify_expr(*value)),
        },
        Expression::Spread(inner) => Expression::Spread(Box::new(simplify_expr(*inner))),
        Expression::Await(inner) => Expression::Await(Box::new(simplify_expr(*inner))),
        Expression::Yield { value, delegate } => Expression::Yield {
            value: Box::new(simplify_expr(*value)),
            delegate,
        },
        other => other,
    }
}

// Simplify `!operand`: double negation, De Morgan's laws, constant folding.
fn simplify_unary_not(operand: Expression) -> Expression {
    // !!x → x (only safe when x is known boolean)
    if let Expression::Unary {
        op: UnaryOp::Not,
        operand: inner,
    } = &operand
    {
        if is_boolean_expr(inner) {
            return *inner.clone();
        }
    }

    // De Morgan's Law: !(a || b) → !a && !b
    if let Expression::Binary {
        op: BinaryOp::Or,
        left,
        right,
    } = &operand
    {
        return Expression::Binary {
            op: BinaryOp::And,
            left: Box::new(negate_expr(*left.clone())),
            right: Box::new(negate_expr(*right.clone())),
        };
    }

    // De Morgan's Law: !(a && b) → !a || !b
    if let Expression::Binary {
        op: BinaryOp::And,
        left,
        right,
    } = &operand
    {
        return Expression::Binary {
            op: BinaryOp::Or,
            left: Box::new(negate_expr(*left.clone())),
            right: Box::new(negate_expr(*right.clone())),
        };
    }

    // !true → false, !false → true
    if let Expression::Value(Value::Constant(Constant::Bool(b))) = &operand {
        return Expression::Value(Value::Constant(Constant::Bool(!b)));
    }

    Expression::Unary {
        op: UnaryOp::Not,
        operand: Box::new(operand),
    }
}

// Simplify binary operations: Or/And identity/absorption, StrictEq/StrictNeq reflexivity.
fn simplify_binary(op: BinaryOp, left: Expression, right: Expression) -> Expression {
    match op {
        // x || false → x, false || x → x, x || true → true, x || x → x
        BinaryOp::Or => {
            if is_falsy(&right) { return left; }
            if is_falsy(&left) { return right; }
            if is_truthy(&right) || is_truthy(&left) {
                return Expression::Value(Value::Constant(Constant::Bool(true)));
            }
            if exprs_equal(&left, &right) { return left; }
        }
        // x && true → x, true && x → x, x && false → false, x && x → x
        BinaryOp::And => {
            if is_truthy(&right) { return left; }
            if is_truthy(&left) { return right; }
            if is_falsy(&right) || is_falsy(&left) {
                return Expression::Value(Value::Constant(Constant::Bool(false)));
            }
            if exprs_equal(&left, &right) { return left; }
        }
        // x === x → true (for simple values)
        BinaryOp::StrictEq => {
            if is_simple_value(&left) && exprs_equal(&left, &right) {
                return Expression::Value(Value::Constant(Constant::Bool(true)));
            }
        }
        // x !== x → false (for simple values)
        BinaryOp::StrictNeq => {
            if is_simple_value(&left) && exprs_equal(&left, &right) {
                return Expression::Value(Value::Constant(Constant::Bool(false)));
            }
        }
        _ => {}
    }

    Expression::Binary {
        op,
        left: Box::new(left),
        right: Box::new(right),
    }
}

// Simplify ternary: constant condition, equal branches, boolean identity/negation.
fn simplify_conditional(
    condition: Expression,
    then_expr: Expression,
    else_expr: Expression,
) -> Expression {
    if is_truthy(&condition) { return then_expr; }
    if is_falsy(&condition) { return else_expr; }

    // c ? x : x → x
    if exprs_equal(&then_expr, &else_expr) { return then_expr; }

    // c ? true : false → c (when c is boolean)
    if is_truthy(&then_expr) && is_falsy(&else_expr) && is_boolean_expr(&condition) {
        return condition;
    }

    // c ? false : true → !c
    if is_falsy(&then_expr) && is_truthy(&else_expr) {
        return negate_expr(condition);
    }

    Expression::Conditional {
        condition: Box::new(condition),
        then_expr: Box::new(then_expr),
        else_expr: Box::new(else_expr),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_double_negation() {
        // !!true → true
        let expr = Expression::Unary {
            op: UnaryOp::Not,
            operand: Box::new(Expression::Unary {
                op: UnaryOp::Not,
                operand: Box::new(Expression::Value(Value::Constant(Constant::Bool(true)))),
            }),
        };

        let result = simplify_expr(expr);
        assert!(matches!(
            result,
            Expression::Value(Value::Constant(Constant::Bool(true)))
        ));
    }

    #[test]
    fn test_de_morgan_or() {
        // !(a || b) → !a && !b
        let a = Expression::Value(Value::Variable("a".to_string()));
        let b = Expression::Value(Value::Variable("b".to_string()));
        let expr = Expression::Unary {
            op: UnaryOp::Not,
            operand: Box::new(Expression::Binary {
                op: BinaryOp::Or,
                left: Box::new(a),
                right: Box::new(b),
            }),
        };

        let result = simplify_expr(expr);
        assert!(matches!(
            result,
            Expression::Binary {
                op: BinaryOp::And,
                ..
            }
        ));
    }

    #[test]
    fn test_de_morgan_and() {
        // !(a && b) → !a || !b
        let a = Expression::Value(Value::Variable("a".to_string()));
        let b = Expression::Value(Value::Variable("b".to_string()));
        let expr = Expression::Unary {
            op: UnaryOp::Not,
            operand: Box::new(Expression::Binary {
                op: BinaryOp::And,
                left: Box::new(a),
                right: Box::new(b),
            }),
        };

        let result = simplify_expr(expr);
        assert!(matches!(
            result,
            Expression::Binary {
                op: BinaryOp::Or,
                ..
            }
        ));
    }

    #[test]
    fn test_or_identity() {
        // x || false → x
        let x = Expression::Value(Value::Variable("x".to_string()));
        let expr = Expression::Binary {
            op: BinaryOp::Or,
            left: Box::new(x.clone()),
            right: Box::new(Expression::Value(Value::Constant(Constant::Bool(false)))),
        };

        let result = simplify_expr(expr);
        assert!(matches!(result, Expression::Value(Value::Variable(ref v)) if v == "x"));
    }

    #[test]
    fn test_and_identity() {
        // x && true → x
        let x = Expression::Value(Value::Variable("x".to_string()));
        let expr = Expression::Binary {
            op: BinaryOp::And,
            left: Box::new(x.clone()),
            right: Box::new(Expression::Value(Value::Constant(Constant::Bool(true)))),
        };

        let result = simplify_expr(expr);
        assert!(matches!(result, Expression::Value(Value::Variable(ref v)) if v == "x"));
    }

    #[test]
    fn test_ternary_constant_condition() {
        // true ? a : b → a
        let a = Expression::Value(Value::Variable("a".to_string()));
        let b = Expression::Value(Value::Variable("b".to_string()));
        let expr = Expression::Conditional {
            condition: Box::new(Expression::Value(Value::Constant(Constant::Bool(true)))),
            then_expr: Box::new(a),
            else_expr: Box::new(b),
        };

        let result = simplify_expr(expr);
        assert!(matches!(result, Expression::Value(Value::Variable(ref v)) if v == "a"));
    }

    #[test]
    fn test_negate_comparison() {
        // !(x === y) → x !== y
        let x = Expression::Value(Value::Variable("x".to_string()));
        let y = Expression::Value(Value::Variable("y".to_string()));
        let inner = Expression::Binary {
            op: BinaryOp::StrictEq,
            left: Box::new(x),
            right: Box::new(y),
        };

        let result = negate_expr(inner);
        assert!(matches!(
            result,
            Expression::Binary {
                op: BinaryOp::StrictNeq,
                ..
            }
        ));
    }
}
