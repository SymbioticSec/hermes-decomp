use crate::ir::{BinaryOp, Constant, Expression, UnaryOp, Value};

pub fn negate_expr(expr: Expression) -> Expression {
    // Avoid double negation
    if let Expression::Unary {
        op: UnaryOp::Not,
        operand,
    } = expr
    {
        return *operand;
    }

    // Negate comparisons
    if let Expression::Binary { op, left, right } = &expr {
        let negated_op = match op {
            BinaryOp::Eq => Some(BinaryOp::Neq),
            BinaryOp::Neq => Some(BinaryOp::Eq),
            BinaryOp::StrictEq => Some(BinaryOp::StrictNeq),
            BinaryOp::StrictNeq => Some(BinaryOp::StrictEq),
            BinaryOp::Lt => Some(BinaryOp::Ge),
            BinaryOp::Le => Some(BinaryOp::Gt),
            BinaryOp::Gt => Some(BinaryOp::Le),
            BinaryOp::Ge => Some(BinaryOp::Lt),
            _ => None,
        };

        if let Some(new_op) = negated_op {
            return Expression::Binary {
                op: new_op,
                left: left.clone(),
                right: right.clone(),
            };
        }
    }

    // Negate boolean constants
    if let Expression::Value(Value::Constant(Constant::Bool(b))) = expr {
        return Expression::Value(Value::Constant(Constant::Bool(!b)));
    }

    // Default: wrap in !
    Expression::Unary {
        op: UnaryOp::Not,
        operand: Box::new(expr),
    }
}

pub fn is_truthy(expr: &Expression) -> bool {
    matches!(
        expr,
        Expression::Value(Value::Constant(Constant::Bool(true)))
    )
}

pub fn is_falsy(expr: &Expression) -> bool {
    matches!(
        expr,
        Expression::Value(Value::Constant(Constant::Bool(false)))
            | Expression::Value(Value::Constant(Constant::Null))
            | Expression::Value(Value::Constant(Constant::Undefined))
            | Expression::Value(Value::Constant(Constant::Integer(0)))
    )
}

pub fn is_boolean_expr(expr: &Expression) -> bool {
    match expr {
        Expression::Value(Value::Constant(Constant::Bool(_))) => true,
        Expression::Unary {
            op: UnaryOp::Not, ..
        } => true,
        Expression::Binary { op, .. } => matches!(
            op,
            BinaryOp::Eq
                | BinaryOp::Neq
                | BinaryOp::StrictEq
                | BinaryOp::StrictNeq
                | BinaryOp::Lt
                | BinaryOp::Le
                | BinaryOp::Gt
                | BinaryOp::Ge
                | BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::In
                | BinaryOp::InstanceOf
        ),
        _ => false,
    }
}

pub use crate::ir::is_simple_value;

// Re-export from centralized location
pub use crate::ir::exprs_equal;
