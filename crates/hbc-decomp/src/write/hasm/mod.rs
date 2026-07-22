// Textual Hermes assembly (HASM): our disasm dialect, parsed back to
// instructions (`parse`) and assembled into new bytecode (`emit`).

use crate::file::Instruction;

pub mod emit;
pub mod parse;

pub use emit::{assemble_function_hasm, assemble_module, emit_hasm_function};
pub use parse::{parse_hasm, parse_hasm_function, parse_hasm_with_context};

// One function in a HASM module.
#[derive(Debug, Clone)]
pub struct HasmFunction {
    pub id: u32,
    pub name: Option<String>,
    pub instructions: Vec<Instruction>,
    pub exception_handlers: Vec<(u32, u32, u32)>,
}

// Parsed HASM module.
#[derive(Debug, Clone)]
pub struct HasmModule {
    pub version: Option<u32>,
    pub functions: Vec<HasmFunction>,
}
