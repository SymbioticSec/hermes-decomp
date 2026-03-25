// Invert if statements with empty then branch.

use crate::ir::{Statement, Expression, UnaryOp};

// Invert if statements: if (!x) {} else { code } -> if (x) { code }
pub(super) fn invert_empty_ifs(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts.into_iter().map(invert_empty_if).collect()
}

fn invert_empty_if(stmt: Statement) -> Statement {
    match stmt {
        Statement::If { condition, then_body, else_body } => {
            let then_body: Vec<_> = then_body.into_iter().map(invert_empty_if).collect();
            let else_body: Vec<_> = else_body.into_iter().map(invert_empty_if).collect();

            // If then is empty but else is not, invert
            if then_body.is_empty() && !else_body.is_empty() {
                Statement::If {
                    condition: negate_condition(condition),
                    then_body: else_body,
                    else_body: vec![],
                }
            } else {
                Statement::If { condition, then_body, else_body }
            }
        }
        Statement::While { condition, body } => Statement::While {
            condition,
            body: body.into_iter().map(invert_empty_if).collect(),
        },
        Statement::Block(inner) => Statement::Block(invert_empty_ifs(inner)),
        _ => stmt,
    }
}

// Negate a condition, simplifying double negations.
fn negate_condition(expr: Expression) -> Expression {
    match expr {
        // !!x -> x (double negation in original, so just negate once)
        Expression::Unary { op: UnaryOp::Not, operand } => *operand,
        // Otherwise wrap in Not
        _ => Expression::unary(UnaryOp::Not, expr),
    }
}
