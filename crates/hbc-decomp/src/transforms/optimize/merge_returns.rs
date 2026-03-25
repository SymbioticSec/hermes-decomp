// Merge return statements in if/else branches.

use crate::ir::Statement;

// Merge return statements that follow each other in if/else branches.
pub(super) fn merge_sequential_returns(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts.into_iter().map(|stmt| {
        match stmt {
            Statement::If { condition, then_body, else_body } => {
                let then_body = merge_sequential_returns(then_body);
                let else_body = merge_sequential_returns(else_body);
                Statement::If { condition, then_body, else_body }
            }
            Statement::While { condition, body } => Statement::While {
                condition,
                body: merge_sequential_returns(body),
            },
            Statement::Block(inner) => Statement::Block(merge_sequential_returns(inner)),
            _ => stmt,
        }
    }).collect()
}
