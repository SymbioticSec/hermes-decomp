// Remove dead `while (x !== x)` loops (NaN check artifacts from Hermes bytecode).
//
// In JavaScript, `x !== x` is only true when x is NaN. Hermes sometimes generates
// these patterns as guard loops that never execute in practice. We remove them.

use crate::ir::{is_nan_check, map_nested_bodies, Statement};

// Remove `while (x !== x) { ... }` dead loops recursively.
// Also detects `while (!(x === x))` which codegen renders identically.
pub(super) fn remove_dead_nan_loops(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts
        .into_iter()
        .filter(|stmt| {
            if let Statement::While { condition, .. } = stmt {
                if is_nan_check(condition) {
                    return false;
                }
            }
            true
        })
        .map(recurse_nan_loops)
        .collect()
}

fn recurse_nan_loops(stmt: Statement) -> Statement {
    map_nested_bodies(stmt, remove_dead_nan_loops)
}
