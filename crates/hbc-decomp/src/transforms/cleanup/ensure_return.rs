// Ensure functions end with a return statement.

use crate::ir::Statement;

// Ensure function ends with a return statement.
pub(super) fn ensure_return(mut stmts: Vec<Statement>) -> Vec<Statement> {
    if !stmts.is_empty() {
        // Check if last statement is already a return
        let needs_return = !ends_with_return(&stmts);

        if needs_return {
            stmts.push(Statement::Return(None));
        }
    }
    stmts
}

fn ends_with_return(stmts: &[Statement]) -> bool {
    if let Some(last) = stmts.last() {
        match last {
            Statement::Return(_) => true,
            Statement::Throw(_) => true,
            Statement::If { then_body, else_body, .. } => {
                ends_with_return(then_body) && ends_with_return(else_body)
            }
            Statement::While { .. } => false, // Loops may not return
            _ => false,
        }
    } else {
        false
    }
}
