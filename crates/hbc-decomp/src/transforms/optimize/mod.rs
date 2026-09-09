pub(crate) mod invert;
pub(crate) mod ternary;
mod dead_assign;
mod dead_bindings;
mod merge_returns;
mod tests;

use crate::ir::Statement;

use invert::invert_empty_ifs;
use ternary::detect_ternaries;
use dead_assign::remove_dead_assignments;
use merge_returns::merge_sequential_returns;

// Dead-store elimination usable on its own late in the pipeline (after naming),
// where a reused slot's dead stores are consecutive and named. Drops a store whose
// value is never read before the next store, keeping a side-effecting call.
pub use dead_bindings::remove_dead_temp_bindings;

pub fn eliminate_dead_stores(stmts: Vec<Statement>) -> Vec<Statement> {
    remove_dead_assignments(stmts)
}

pub fn optimize_statements(stmts: Vec<Statement>) -> Vec<Statement> {
    let stmts = invert_empty_ifs(stmts);
    let stmts = detect_ternaries(stmts);
    let stmts = remove_dead_assignments(stmts);

    merge_sequential_returns(stmts)
}
