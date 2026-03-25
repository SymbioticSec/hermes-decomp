use crate::ir::{AssignTarget, Constant, Expression, PropertyKey, Statement, Value};

// Extract property access pattern from a statement.
pub fn extract_property_access(
    stmt: &Statement,
) -> Option<(Expression, PropertyKey, AssignTarget)> {
    match stmt {
        Statement::Assign { target, value } => {
            if let Expression::Member {
                object,
                property,
                optional: false,
            } = value
            {
                return Some((*object.clone(), property.clone(), target.clone()));
            }
            None
        }
        Statement::Let { name, value, .. } => {
            if let Expression::Member {
                object,
                property,
                optional: false,
            } = value
            {
                return Some((
                    *object.clone(),
                    property.clone(),
                    AssignTarget::Variable(name.clone()),
                ));
            }
            None
        }
        _ => None,
    }
}

// Get integer index from a property key.
pub fn get_index(key: &PropertyKey) -> Option<i64> {
    match key {
        PropertyKey::Index(idx) => Some(*idx),
        PropertyKey::Computed(expr) => match expr.as_ref() {
            Expression::Value(Value::Constant(Constant::Integer(i))) => Some(*i as i64),
            Expression::Value(Value::Constant(Constant::Number(n))) => {
                if n.fract() == 0.0 && *n >= 0.0 && *n < i64::MAX as f64 {
                    Some(*n as i64)
                } else {
                    None
                }
            }
            _ => None,
        },
        _ => None,
    }
}

// Re-export from centralized location
pub use crate::ir::exprs_equal;
