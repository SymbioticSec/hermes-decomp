use super::expressions::simplify_expr;
use crate::ir::{Constant, Expression, Statement, Value};

// Apply advanced logic simplifications to statements.
pub fn simplify_logic_advanced(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts.into_iter().map(simplify_stmt).collect()
}

fn simplify_stmt(stmt: Statement) -> Statement {
    match stmt {
        Statement::Expr(e) => Statement::Expr(simplify_expr(e)),
        Statement::Let { name, value, kind } => Statement::Let {
            name,
            value: simplify_expr(value),
            kind,
        },
        Statement::Assign { target, value } => Statement::Assign {
            target,
            value: simplify_expr(value),
        },
        Statement::Return(Some(e)) => Statement::Return(Some(simplify_expr(e))),
        Statement::Throw(e) => Statement::Throw(simplify_expr(e)),
        Statement::If {
            condition,
            then_body,
            else_body,
        } => {
            let simplified_cond = simplify_expr(condition);

            // If condition is constant, we can eliminate the branch
            match &simplified_cond {
                Expression::Value(Value::Constant(Constant::Bool(true))) => {
                    // Always true - use then_body only
                    if then_body.len() == 1 {
                        return simplify_stmt(then_body.into_iter().next().unwrap());
                    }
                    return Statement::Block(simplify_logic_advanced(then_body));
                }
                Expression::Value(Value::Constant(Constant::Bool(false))) => {
                    // Always false - use else_body only
                    if else_body.is_empty() {
                        return Statement::Comment("// eliminated: always false".to_string());
                    }
                    if else_body.len() == 1 {
                        return simplify_stmt(else_body.into_iter().next().unwrap());
                    }
                    return Statement::Block(simplify_logic_advanced(else_body));
                }
                _ => {}
            }

            Statement::If {
                condition: simplified_cond,
                then_body: simplify_logic_advanced(then_body),
                else_body: simplify_logic_advanced(else_body),
            }
        }
        Statement::While { condition, body } => Statement::While {
            condition: simplify_expr(condition),
            body: simplify_logic_advanced(body),
        },
        Statement::DoWhile { body, condition } => Statement::DoWhile {
            body: simplify_logic_advanced(body),
            condition: simplify_expr(condition),
        },
        Statement::For {
            init,
            condition,
            update,
            body,
        } => Statement::For {
            init: init.map(|s| Box::new(simplify_stmt(*s))),
            condition: condition.map(simplify_expr),
            update: update.map(|s| Box::new(simplify_stmt(*s))),
            body: simplify_logic_advanced(body),
        },
        Statement::ForOf {
            variable,
            iterable,
            body,
        } => Statement::ForOf {
            variable,
            iterable: simplify_expr(iterable),
            body: simplify_logic_advanced(body),
        },
        Statement::ForIn {
            variable,
            object,
            body,
        } => Statement::ForIn {
            variable,
            object: simplify_expr(object),
            body: simplify_logic_advanced(body),
        },
        Statement::TryCatch {
            try_body,
            catch_param,
            catch_body,
            finally_body,
        } => Statement::TryCatch {
            try_body: simplify_logic_advanced(try_body),
            catch_param,
            catch_body: simplify_logic_advanced(catch_body),
            finally_body: simplify_logic_advanced(finally_body),
        },
        Statement::Switch {
            discriminant,
            cases,
            default,
        } => Statement::Switch {
            discriminant: simplify_expr(discriminant),
            cases: cases
                .into_iter()
                .map(|(e, stmts)| (simplify_expr(e), simplify_logic_advanced(stmts)))
                .collect(),
            default: default.map(simplify_logic_advanced),
        },
        Statement::Block(stmts) => Statement::Block(simplify_logic_advanced(stmts)),
        other => other,
    }
}
