use crate::ir::{AssignTarget, BinaryOp, Constant, Expression, PropertyKey, Statement, UnaryOp, Value};

pub fn expr_uses_register(expr: &Expression, reg: u32) -> bool {
    match expr {
        Expression::Value(Value::Register(r)) => *r == reg,
        Expression::Binary { left, right, .. } => {
            expr_uses_register(left, reg) || expr_uses_register(right, reg)
        }
        Expression::Unary { operand, .. } => expr_uses_register(operand, reg),
        Expression::Member {
            object, property, ..
        } => expr_uses_register(object, reg) || property_key_uses_register(property, reg),
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            expr_uses_register(callee, reg) || arguments.iter().any(|a| expr_uses_register(a, reg))
        }
        Expression::Object { properties } => properties
            .iter()
            .any(|p| expr_uses_register(&p.value, reg) || property_key_uses_register(&p.key, reg)),
        Expression::Array { elements } => elements
            .iter()
            .flatten()
            .any(|e| expr_uses_register(e, reg)),
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_uses_register(condition, reg)
                || expr_uses_register(then_expr, reg)
                || expr_uses_register(else_expr, reg)
        }
        Expression::Function { .. } => false, // Functions don't directly use registers
        _ => false,
    }
}

pub fn property_key_uses_register(key: &PropertyKey, reg: u32) -> bool {
    match key {
        PropertyKey::Computed(e) => expr_uses_register(e, reg),
        _ => false,
    }
}

pub fn stmt_uses_register(stmt: &Statement, reg: u32) -> bool {
    match stmt {
        Statement::Assign { target, value } => {
            let target_uses = match target {
                AssignTarget::Member { object, .. } => expr_uses_register(object, reg),
                AssignTarget::Index { object, key } => {
                    expr_uses_register(object, reg) || expr_uses_register(key, reg)
                }
                _ => false,
            };
            target_uses || expr_uses_register(value, reg)
        }
        Statement::Expr(e) => expr_uses_register(e, reg),
        Statement::Return(Some(e)) | Statement::Throw(e) => expr_uses_register(e, reg),
        Statement::Return(None) => false,
        Statement::If { condition, .. } => expr_uses_register(condition, reg),
        Statement::While { condition, .. } => expr_uses_register(condition, reg),
        Statement::Block(stmts) => stmts.iter().any(|s| stmt_uses_register(s, reg)),
        _ => false,
    }
}

pub fn target_to_key(target: &AssignTarget) -> Option<String> {
    match target {
        AssignTarget::Register(r) => Some(format!("r{r}")),
        AssignTarget::Variable(name) => Some(name.clone()),
        AssignTarget::ClosureVar { slot, level, .. } => Some(format!("closure_{level}_{slot}")),
        _ => None,
    }
}

pub fn get_value_name(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Value(Value::Variable(n)) => Some(n.clone()),
        Expression::Value(Value::Register(r)) => Some(format!("r{r}")),
        Expression::Value(Value::Parameter(idx)) => Some(format!("arg{idx}")),
        _ => None,
    }
}

pub fn extract_function_id(expr: &Expression) -> Option<u32> {
    match expr {
        Expression::Function { id, .. } => Some(id.0),
        _ => None,
    }
}

// Check if two expressions are structurally equal.
// Used by pattern detection, destructuring, and logic simplification passes.
pub fn exprs_equal(a: &Expression, b: &Expression) -> bool {
    match (a, b) {
        (Expression::Value(v1), Expression::Value(v2)) => v1 == v2,
        (
            Expression::Member {
                object: o1,
                property: p1,
                optional: opt1,
            },
            Expression::Member {
                object: o2,
                property: p2,
                optional: opt2,
            },
        ) => opt1 == opt2 && property_keys_equal(p1, p2) && exprs_equal(o1, o2),
        (
            Expression::Call {
                callee: c1,
                arguments: args1,
            },
            Expression::Call {
                callee: c2,
                arguments: args2,
            },
        ) => {
            exprs_equal(c1, c2)
                && args1.len() == args2.len()
                && args1
                    .iter()
                    .zip(args2.iter())
                    .all(|(a, b)| exprs_equal(a, b))
        }
        (
            Expression::Binary {
                op: op1,
                left: l1,
                right: r1,
            },
            Expression::Binary {
                op: op2,
                left: l2,
                right: r2,
            },
        ) => op1 == op2 && exprs_equal(l1, l2) && exprs_equal(r1, r2),
        (
            Expression::Unary {
                op: op1,
                operand: o1,
            },
            Expression::Unary {
                op: op2,
                operand: o2,
            },
        ) => op1 == op2 && exprs_equal(o1, o2),
        (
            Expression::Conditional {
                condition: c1,
                then_expr: t1,
                else_expr: e1,
            },
            Expression::Conditional {
                condition: c2,
                then_expr: t2,
                else_expr: e2,
            },
        ) => exprs_equal(c1, c2) && exprs_equal(t1, t2) && exprs_equal(e1, e2),
        (Expression::Array { elements: e1 }, Expression::Array { elements: e2 }) => {
            e1.len() == e2.len()
                && e1.iter().zip(e2.iter()).all(|(a, b)| match (a, b) {
                    (Some(ea), Some(eb)) => exprs_equal(ea, eb),
                    (None, None) => true,
                    _ => false,
                })
        }
        _ => false,
    }
}

// Apply a transformation function to all nested statement bodies in a statement.
// Handles If, While, DoWhile, For, ForIn, ForOf, TryCatch, Switch, and Block.
// Non-body fields (conditions, expressions) are preserved unchanged.
pub fn map_nested_bodies(stmt: Statement, mut f: impl FnMut(Vec<Statement>) -> Vec<Statement>) -> Statement {
    match stmt {
        Statement::If { condition, then_body, else_body } => Statement::If {
            condition,
            then_body: f(then_body),
            else_body: f(else_body),
        },
        Statement::While { condition, body } => Statement::While {
            condition,
            body: f(body),
        },
        Statement::DoWhile { body, condition } => Statement::DoWhile {
            body: f(body),
            condition,
        },
        Statement::For { init, condition, update, body } => Statement::For {
            init,
            condition,
            update,
            body: f(body),
        },
        Statement::ForIn { variable, object, body } => Statement::ForIn {
            variable,
            object,
            body: f(body),
        },
        Statement::ForOf { variable, iterable, body } => Statement::ForOf {
            variable,
            iterable,
            body: f(body),
        },
        Statement::TryCatch { try_body, catch_param, catch_body, finally_body } => Statement::TryCatch {
            try_body: f(try_body),
            catch_param,
            catch_body: f(catch_body),
            finally_body: f(finally_body),
        },
        Statement::Switch { discriminant, cases, default } => Statement::Switch {
            discriminant,
            cases: cases.into_iter().map(|(e, stmts)| (e, f(stmts))).collect(),
            default: default.map(&mut f),
        },
        Statement::Block(stmts) => Statement::Block(f(stmts)),
        other => other,
    }
}

pub fn map_nested_bodies_mut(stmt: &mut Statement, mut f: impl FnMut(Vec<Statement>) -> Vec<Statement>) {
    match stmt {
        Statement::If { then_body, else_body, .. } => {
            let t = std::mem::take(then_body);
            *then_body = f(t);
            let e = std::mem::take(else_body);
            *else_body = f(e);
        }
        Statement::While { body, .. } | Statement::DoWhile { body, .. }
        | Statement::For { body, .. } | Statement::ForIn { body, .. }
        | Statement::ForOf { body, .. } => {
            let b = std::mem::take(body);
            *body = f(b);
        }
        Statement::Block(inner) => {
            let b = std::mem::take(inner);
            *inner = f(b);
        }
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
            let t = std::mem::take(try_body);
            *try_body = f(t);
            let c = std::mem::take(catch_body);
            *catch_body = f(c);
            let fb = std::mem::take(finally_body);
            *finally_body = f(fb);
        }
        Statement::Switch { cases, default, .. } => {
            for (_, body) in cases.iter_mut() {
                let b = std::mem::take(body);
                *body = f(b);
            }
            if let Some(d) = default {
                let b = std::mem::take(d);
                *d = f(b);
            }
        }
        _ => {}
    }
}

pub fn property_keys_equal(a: &PropertyKey, b: &PropertyKey) -> bool {
    match (a, b) {
        (PropertyKey::String(s1), PropertyKey::String(s2)) => s1 == s2,
        (PropertyKey::Ident(s1), PropertyKey::Ident(s2)) => s1 == s2,
        (PropertyKey::Computed(e1), PropertyKey::Computed(e2)) => exprs_equal(e1, e2),
        _ => false,
    }
}

// Check if a statement has side effects.
// Canonical implementation, uses `Expression::has_side_effects()` for expression-level checks.
pub fn stmt_has_side_effects(stmt: &Statement) -> bool {
    match stmt {
        Statement::Assign { value, .. } => value.has_side_effects(),
        Statement::Expr(e) => e.has_side_effects(),
        Statement::Return(_) | Statement::Throw(_) => true,
        Statement::If { .. } | Statement::While { .. } | Statement::Block(_) => true,
        Statement::Switch { .. } | Statement::For { .. } => true,
        Statement::ForIn { .. } | Statement::ForOf { .. } | Statement::DoWhile { .. } => true,
        Statement::TryCatch { .. } => true,
        Statement::Comment(_) => false,
        Statement::Let { value, .. } => value.has_side_effects(),
        _ => true,
    }
}

pub fn is_simple_value(expr: &Expression) -> bool {
    matches!(
        expr,
        Expression::Value(Value::Constant(_))
            | Expression::Value(Value::Register(_))
            | Expression::Value(Value::Variable(_))
            | Expression::Value(Value::This)
            | Expression::Value(Value::Global)
    )
}

// Check if a condition is `x !== x` or `!(x === x)` (NaN check pattern).
// This pattern arises from Hermes bytecode and indicates dead code.
pub fn is_nan_check(condition: &Expression) -> bool {
    if let Expression::Binary { op: BinaryOp::StrictNeq, left, right } = condition {
        if left == right {
            return true;
        }
    }
    if let Expression::Unary { op: UnaryOp::Not, operand } = condition {
        if let Expression::Binary { op: BinaryOp::StrictEq, left, right } = operand.as_ref() {
            if left == right {
                return true;
            }
        }
    }
    false
}

pub fn is_undefined_expr(expr: &Expression) -> bool {
    matches!(expr, Expression::Value(Value::Constant(Constant::Undefined)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Constant;

    fn make_body(n: i32) -> Vec<Statement> {
        vec![Statement::Expr(Expression::Value(Value::Constant(Constant::Integer(n))))]
    }

    #[test]
    fn test_map_nested_bodies_if() {
        let stmt = Statement::If {
            condition: Expression::Value(Value::Constant(Constant::Bool(true))),
            then_body: make_body(1),
            else_body: make_body(2),
        };

        let mut called = 0;
        let result = map_nested_bodies(stmt, |body| {
            called += 1;
            body
        });
        assert_eq!(called, 2, "Should map both then and else bodies");
        assert!(matches!(result, Statement::If { .. }));
    }

    #[test]
    fn test_map_nested_bodies_while() {
        let stmt = Statement::While {
            condition: Expression::Value(Value::Constant(Constant::Bool(true))),
            body: make_body(1),
        };

        let mut called = 0;
        let result = map_nested_bodies(stmt, |body| {
            called += 1;
            body
        });
        assert_eq!(called, 1);
        assert!(matches!(result, Statement::While { .. }));
    }

    #[test]
    fn test_map_nested_bodies_trycatch() {
        let stmt = Statement::TryCatch {
            try_body: make_body(1),
            catch_param: None,
            catch_body: make_body(2),
            finally_body: make_body(3),
        };

        let mut called = 0;
        let result = map_nested_bodies(stmt, |body| {
            called += 1;
            body
        });
        assert_eq!(called, 3, "Should map try, catch, and finally bodies");
        assert!(matches!(result, Statement::TryCatch { .. }));
    }

    #[test]
    fn test_map_nested_bodies_leaf() {
        let stmt = Statement::Return(None);
        let mut called = 0;
        let result = map_nested_bodies(stmt, |body| {
            called += 1;
            body
        });
        assert_eq!(called, 0, "Leaf statements should not invoke the callback");
        assert!(matches!(result, Statement::Return(None)));
    }

    #[test]
    fn test_map_nested_bodies_transforms_content() {
        // Verify that the function's output is actually used (bodies replaced)
        let stmt = Statement::Block(make_body(1));
        let result = map_nested_bodies(stmt, |_| Vec::new());
        if let Statement::Block(body) = result {
            assert!(body.is_empty(), "Body should have been replaced with empty vec");
        } else {
            panic!("Expected Block statement");
        }
    }

    #[test]
    fn test_map_nested_bodies_mut_if() {
        let mut stmt = Statement::If {
            condition: Expression::Value(Value::Constant(Constant::Bool(true))),
            then_body: make_body(1),
            else_body: make_body(2),
        };

        map_nested_bodies_mut(&mut stmt, |_| Vec::new());

        if let Statement::If { then_body, else_body, .. } = &stmt {
            assert!(then_body.is_empty());
            assert!(else_body.is_empty());
        } else {
            panic!("Expected If statement");
        }
    }
}
