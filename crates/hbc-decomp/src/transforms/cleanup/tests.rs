#[cfg(test)]
mod tests {
    use crate::transforms::cleanup::undefined::remove_undefined_initializations;
    use crate::transforms::cleanup::redundant::remove_redundant_assignments;
    use crate::ir::{Statement, Expression, Value, Constant};

    #[test]
    fn test_remove_undefined_init() {
        let stmts = vec![
            Statement::assign_reg(0, Expression::constant(Constant::Undefined)),
            Statement::assign_reg(0, Expression::constant(Constant::Integer(42))),
        ];

        let result = remove_undefined_initializations(stmts);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_remove_self_assignment() {
        let stmts = vec![
            Statement::assign_reg(0, Expression::Value(Value::Register(0))),
            Statement::assign_reg(1, Expression::constant(Constant::Integer(42))),
        ];

        let result = remove_redundant_assignments(stmts);
        assert_eq!(result.len(), 1);
    }
}
