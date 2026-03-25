// Hermes Bytecode Decompiler Library
//
// This library provides tools for parsing, disassembling, and decompiling
// Hermes bytecode files (`.hbc`) used by React Native applications.
//
// # Architecture
//
// The decompilation pipeline consists of several phases:
//
// 1. **Parsing** (`file`, `format`, `opcode`): Parse the binary bytecode format
// 2. **IR Generation** (`ir`): Convert bytecode to an intermediate representation
// 3. **Analysis** (`analysis`): Analyze the IR (liveness, reaching defs, structure)
// 4. **Transformation** (`transforms`): Optimize and simplify the IR
// 5. **Code Generation** (`transforms::codegen`): Generate JavaScript-like output
//
// # Example
//
// ```no_run
// use hbc::{Decompiler, DecompileOptionsV2};
//
// let bytes = std::fs::read("app.hbc").unwrap();
// let decompiler = Decompiler::new(&bytes);
//
// let options = DecompileOptionsV2::default();
// let output = decompiler.decompile_function(0, &options).unwrap();
// println!("{}", output);
// ```

// Suppress collapsible_match/collapsible_if: fixing these requires unstable let-chains (RFC #53667)
#![allow(clippy::collapsible_match, clippy::collapsible_if)]

pub mod debug;
pub mod disasm;
pub mod error;
pub mod file;
pub mod format;
pub mod io;
pub mod opcode;
pub mod pipeline;
pub mod util;

pub mod analysis;
pub mod constants;
pub mod ir;
pub mod transforms;

pub use disasm::{collect_label_offsets, disassemble_all, disassemble_function, DisasmOptions};
pub use error::{Error, Result};
pub use file::{BytecodeFile, Instruction, SectionInfo};
pub use format::{BytecodeHeader, FunctionHeader, FunctionHeaderLayout, HeaderLayout};
pub use opcode::{BytecodeFormat, Operand, OperandType, OperandValue};
pub use util::{escape_js_string, is_valid_identifier};

pub use ir::{
    AssignTarget, BasicBlock, BinaryOp, BlockId, Constant, Expression, FunctionId, IRBuilder,
    IRBuilderOptions, Statement, Terminator, UnaryOp, Value, CFG,
};

pub use analysis::{
    analyze_registers, generate_name, rename_registers, resolve_closures, ClosureContext,
    ClosureInfo, ClosureSlotValue, DependencyTree, MetroModule, MetroRegistry, Structure,
    StructureAnalysis,
};

pub use transforms::{
    cleanup_statements, detect_class_patterns, detect_destructuring, detect_patterns,
    inline_expressions, optimize_statements, propagate, simplify_expr, simplify_stmt, Codegen,
    CodegenOptions, PropagationConfig,
};

pub use debug::{DebugInfo, ScopeDescriptor, SourceLocation};

pub use pipeline::{
    analyze_module, build_closure_context_from_file as build_closure_context,
    decompile_all_v2_with_closures, decompile_function_v2, decompile_function_v2_with_context,
    generate_ir, DecompileOptionsV2, Decompiler, PipelineContext,
};
