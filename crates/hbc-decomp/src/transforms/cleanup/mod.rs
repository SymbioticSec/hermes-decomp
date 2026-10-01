pub(crate) mod advanced;
mod chain;
mod dead_loops;
mod empty_blocks;
mod ensure_return;
pub(crate) mod redundant;
#[cfg(test)]
mod tests;
pub(crate) mod undefined;

use crate::ir::Statement;

use chain::fold_chain_assignments;
use dead_loops::remove_dead_nan_loops;
use empty_blocks::remove_empty_blocks;
use ensure_return::ensure_return;
use redundant::remove_redundant_assignments;
use undefined::remove_undefined_initializations;

pub fn cleanup_statements(stmts: Vec<Statement>) -> Vec<Statement> {
    let stmts = remove_undefined_initializations(stmts);
    let stmts = remove_redundant_assignments(stmts);
    let stmts = fold_chain_assignments(stmts);
    let stmts = remove_dead_nan_loops(stmts);
    let stmts = remove_empty_blocks(stmts);

    ensure_return(stmts)
}
