mod expressions;
pub mod jsx;
mod logic_short_circuit;
mod loops;
pub mod utils;

use crate::ir::Statement;

pub use expressions::{
    detect_logical_patterns, detect_nullish_coalescing, detect_optional_chaining,
    detect_string_concat,
};
pub use loops::{detect_for_in_loops, detect_for_loops, detect_for_of_loops, detect_legacy_for_of};
pub use logic_short_circuit::detect_short_circuit_logic;
pub use jsx::reconstruct_jsx;

pub fn detect_patterns(stmts: Vec<Statement>) -> Vec<Statement> {
    let stmts = detect_short_circuit_logic(stmts);
    let stmts = detect_for_of_loops(stmts);
    let stmts = detect_for_in_loops(stmts);
    let stmts = detect_nullish_coalescing(stmts);
    let stmts = detect_optional_chaining(stmts);
    let stmts = detect_for_loops(stmts);
    let stmts = detect_logical_patterns(stmts);
    
    let mut final_stmts = detect_string_concat(stmts);

    // Apply JSX reconstruction
    let mut jsx_pass = jsx::JSXReconstructor::new();
    use crate::ir::MutVisitor;
    jsx_pass.visit_statement_list(&mut final_stmts);

    final_stmts
}
