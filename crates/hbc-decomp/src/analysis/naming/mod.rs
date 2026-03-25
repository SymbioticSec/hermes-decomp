mod generation;
mod registers;
mod renaming;

pub use generation::generate_name;
pub use registers::{analyze_registers, RegisterInfo, RegisterRole};
pub use renaming::{rename_registers, rename_variables_in_stmts};
