#[cfg(test)]
mod tests {
    use crate::transforms::optimize::invert::invert_empty_ifs;
    use crate::transforms::optimize::ternary::detect_ternaries;
    use crate::ir::{Statement, Expression, AssignTarget, Value, UnaryOp, Constant};

    #[test]
    fn test_invert_empty_if() {
        let stmt = Statement::If {
            condition: Expression::Value(Value::Register(0)),
            then_body: vec![],
            else_body: vec![Statement::Return(Some(Expression::constant(Constant::Integer(1))))],
        };

        let result = invert_empty_ifs(vec![stmt]);
        let result = &result[0];

        if let Statement::If { condition, then_body, else_body } = result {
            assert!(!then_body.is_empty());
            assert!(else_body.is_empty());
            // Condition should be negated
            assert!(matches!(condition, Expression::Unary { op: UnaryOp::Not, .. }));
        } else {
            panic!("Expected If statement");
        }
    }

    #[test]
    fn test_detect_ternary() {
        let stmts = vec![Statement::If {
            condition: Expression::Value(Value::Register(0)),
            then_body: vec![Statement::assign_reg(1, Expression::constant(Constant::Integer(10)))],
            else_body: vec![Statement::assign_reg(1, Expression::constant(Constant::Integer(20)))],
        }];

        let result = detect_ternaries(stmts);
        let result = &result[0];

        if let Statement::Assign { target: AssignTarget::Register(1), value } = result {
            assert!(matches!(value, Expression::Conditional { .. }));
        } else {
            panic!("Expected ternary assignment");
        }
    }
}
