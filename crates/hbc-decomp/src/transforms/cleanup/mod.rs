pub(crate) mod advanced;
mod chain;
mod dead_loops;
mod empty_blocks;
pub(crate) mod undefined;
pub(crate) mod redundant;
mod ensure_return;
mod tests;

use crate::ir::Statement;

use chain::fold_chain_assignments;
use dead_loops::remove_dead_nan_loops;
use empty_blocks::remove_empty_blocks;
use undefined::remove_undefined_initializations;
use redundant::remove_redundant_assignments;
use ensure_return::ensure_return;

pub fn cleanup_statements(stmts: Vec<Statement>) -> Vec<Statement> {
    let stmts = remove_undefined_initializations(stmts);
    let stmts = remove_redundant_assignments(stmts);
    let stmts = fold_chain_assignments(stmts);
    let stmts = remove_dead_nan_loops(stmts);
    let stmts = remove_empty_blocks(stmts);

    ensure_return(stmts)
}
