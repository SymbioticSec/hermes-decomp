mod builtin_guard;
mod dead_assign;
mod dead_bindings;
pub(crate) mod invert;
mod merge_returns;
mod switch_clobber;
pub(crate) mod ternary;
#[cfg(test)]
mod tests;

use crate::ir::Statement;

use dead_assign::remove_dead_assignments;
use invert::invert_empty_ifs;
use merge_returns::merge_sequential_returns;
use ternary::detect_ternaries;

// Dead-store elimination usable on its own late in the pipeline (after naming),
// where a reused slot's dead stores are consecutive and named. Drops a store whose
// value is never read before the next store, keeping a side-effecting call.
pub use builtin_guard::fold_builtin_guards;
pub use dead_bindings::{remove_dead_temp_bindings, remove_dead_temp_bindings_keeping};
pub use switch_clobber::repair_switch_clobbers;

pub fn eliminate_dead_stores(stmts: Vec<Statement>) -> Vec<Statement> {
    remove_dead_assignments(stmts)
}

pub fn optimize_statements(stmts: Vec<Statement>) -> Vec<Statement> {
    let stmts = invert_empty_ifs(stmts);
    let stmts = detect_ternaries(stmts);
    let stmts = remove_dead_assignments(stmts);

    merge_sequential_returns(stmts)
}
