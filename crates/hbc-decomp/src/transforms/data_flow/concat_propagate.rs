use std::collections::BTreeMap;
use crate::ir::{AssignTarget, BinaryOp, Constant, Expression, Statement, Value, MutVisitor};

// Propagates string concatenations across multiple assignments to reconstruct
// complete template literals.
//
// Example:
// ```javascript
// let r1 = "User ";
// let r2 = r1 + name;
// let r3 = r2 + " signed in.";
// console.log(r3);
// ```
// Becomes:
// ```javascript
// ...
// console.log(`User ${name} signed in.`);
// ```
pub fn propagate_concatenation(mut stmts: Vec<Statement>) -> Vec<Statement> {
    let mut visitor = ConcatPropagator::new();
    visitor.visit_statement_list(&mut stmts);
    stmts
}

struct ConcatPropagator {
    // Maps a register to its current known TemplateLiteral state
    tracked_strings: BTreeMap<u32, Expression>,
}

impl ConcatPropagator {
    fn new() -> Self {
        Self {
            tracked_strings: BTreeMap::new(),
        }
    }

    fn build_template_literal(s: &str) -> Expression {
        Expression::TemplateLiteral {
            quasis: vec![s.to_string()],
            expressions: vec![],
        }
    }

    fn append_to_template(base: &Expression, addition: &Expression) -> Option<Expression> {
        if let Expression::TemplateLiteral { quasis, expressions } = base {
            let mut new_quasis = quasis.clone();
            let mut new_exprs = expressions.clone();

            match addition {
                Expression::Value(Value::Constant(Constant::String(s))) => {
                    // Append to the last quasi
                    if let Some(last) = new_quasis.last_mut() {
                        last.push_str(s);
                    } else {
                        new_quasis.push(s.clone());
                    }
                }
                _ => {
                    // Add as a new expression
                    new_exprs.push(addition.clone());
                    new_quasis.push(String::new()); // Followed by an empty quasi
                }
            }

            Some(Expression::TemplateLiteral {
                quasis: new_quasis,
                expressions: new_exprs,
            })
        } else {
            None
        }
    }

    fn get_register(expr: &Expression) -> Option<u32> {
        if let Expression::Value(Value::Register(r)) = expr {
            Some(*r)
        } else {
            None
        }
    }

    fn is_string_or_template(expr: &Expression) -> bool {
        matches!(expr, Expression::TemplateLiteral { .. } | Expression::Value(Value::Constant(Constant::String(_))))
    }

    fn expr_to_template_base(expr: &Expression) -> Expression {
        match expr {
            Expression::TemplateLiteral { .. } => expr.clone(),
            Expression::Value(Value::Constant(Constant::String(s))) => Self::build_template_literal(s),
            _ => {
                Expression::TemplateLiteral {
                    quasis: vec![String::new(), String::new()],
                    expressions: vec![expr.clone()],
                }
            }
        }
    }
}

impl MutVisitor for ConcatPropagator {
    fn visit_statement_list(&mut self, stmts: &mut Vec<Statement>) {
        // We process top-down linearly to follow data flow
        for stmt in stmts.iter_mut() {
            self.visit_statement(stmt);
        }
    }

    fn visit_statement(&mut self, stmt: &mut Statement) {
        match stmt {
            Statement::Assign { target: AssignTarget::Register(r), value } => {
                // First, try replacing variables inside the value using walk_expression
                self.walk_expression(value);

                // Now analyze if this assignment creates or extends a tracked string
                match &*value {
                    Expression::Value(Value::Constant(Constant::String(s))) => {
                        // Starts a string chain
                        self.tracked_strings.insert(*r, Self::build_template_literal(s));
                    }
                    Expression::Binary { op: BinaryOp::Add, left, right } => {
                        // Check if either side guarantees this is a string concatenation
                        if Self::is_string_or_template(left) || Self::is_string_or_template(right) {
                            let base = Self::expr_to_template_base(left);
                            if let Some(new_tmpl) = Self::append_to_template(&base, right) {
                                self.tracked_strings.insert(*r, new_tmpl.clone());
                                *value = new_tmpl; // Update AST inline
                            } else {
                                self.tracked_strings.remove(r);
                            }
                        } else {
                            self.tracked_strings.remove(r);
                        }
                    }
                    Expression::TemplateLiteral { .. } => {
                        // Re-assignment of a tracked template literal
                        self.tracked_strings.insert(*r, value.clone());
                    }
                    _ => {
                        // Destroy tracking for this register
                        self.tracked_strings.remove(r);
                    }
                }
            }
            Statement::Assign { target, value } => {
                self.visit_assign_target(target);
                self.walk_expression(value);
            }
            _ => self.walk_statement(stmt),
        }
    }

    fn visit_expression(&mut self, expr: &mut Expression) {
        // Replace uses of the tracked string
        if let Some(r) = Self::get_register(expr) {
            if let Some(tmpl) = self.tracked_strings.get(&r) {
                *expr = tmpl.clone();
                return;
            }
        }
        
        self.walk_expression(expr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Constant;

    #[test]
    fn test_propagate_string_concat() {
        // r1 = "User "
        // r2 = r1 + x
        // r3 = r2 + " connected"
        // return r3
        
        let stmts = vec![
            Statement::assign_reg(1, Expression::constant(Constant::String("User ".to_string()))),
            Statement::assign_reg(2, Expression::binary(
                BinaryOp::Add,
                Expression::register(1),
                Expression::Value(Value::Variable("x".to_string())),
            )),
            Statement::assign_reg(3, Expression::binary(
                BinaryOp::Add,
                Expression::register(2),
                Expression::constant(Constant::String(" connected".to_string())),
            )),
            Statement::Return(Some(Expression::register(3))),
        ];

        let result = propagate_concatenation(stmts);
        
        // r3 should be replaced in the return statement
        if let Statement::Return(Some(Expression::TemplateLiteral { quasis, expressions })) = &result[3] {
            assert_eq!(quasis, &vec!["User ".to_string(), " connected".to_string()]);
            assert_eq!(expressions.len(), 1);
            if let Expression::Value(Value::Variable(v)) = &expressions[0] {
                assert_eq!(v, "x");
            } else {
                panic!("Expected variable x");
            }
        } else {
            panic!("Expected template literal in return array");
        }
    }
}
