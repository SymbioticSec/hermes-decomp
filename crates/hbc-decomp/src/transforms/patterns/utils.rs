use crate::ir::{Constant, Expression, Value};

// Check if an expression is null.
pub fn is_null(expr: &Expression) -> bool {
    matches!(expr, Expression::Value(Value::Constant(Constant::Null)))
}

// Check if an expression is undefined.
// Re-exported from `ir::is_undefined_expr`.
pub use crate::ir::is_undefined_expr as is_undefined;

// Check if an expression is null or undefined.
pub fn is_null_or_undefined(expr: &Expression) -> bool {
    is_null(expr) || crate::ir::is_undefined_expr(expr)
}

// Check if an expression is zero (integer 0 or float 0.0).
pub fn is_zero(expr: &Expression) -> bool {
    match expr {
        Expression::Value(Value::Constant(Constant::Integer(0))) => true,
        Expression::Value(Value::Constant(Constant::Number(n))) => *n == 0.0,
        _ => false,
    }
}

// Re-export from centralized location
pub use crate::ir::exprs_equal;
