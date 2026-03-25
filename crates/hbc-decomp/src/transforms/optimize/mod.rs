pub(crate) mod invert;
pub(crate) mod ternary;
mod dead_assign;
mod merge_returns;
mod tests;

use crate::ir::Statement;

use invert::invert_empty_ifs;
use ternary::detect_ternaries;
use dead_assign::remove_dead_assignments;
use merge_returns::merge_sequential_returns;

pub fn optimize_statements(stmts: Vec<Statement>) -> Vec<Statement> {
    let stmts = invert_empty_ifs(stmts);
    let stmts = detect_ternaries(stmts);
    let stmts = remove_dead_assignments(stmts);

    merge_sequential_returns(stmts)
}
