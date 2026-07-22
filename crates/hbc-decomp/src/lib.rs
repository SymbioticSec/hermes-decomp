// Hermes Bytecode Decompiler Library
//
// This library provides tools for parsing, disassembling, decompiling, and
// (in progress) assembling / patching Hermes bytecode files (`.hbc`) used by
// React Native applications.
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
// The **write path** is separate (`write`): encode instructions, assemble HASM,
// patch existing bundles, serialize full `.hbc` images. It does *not* recompile
// decompiled JavaScript. See repository `ROADMAP.md`.
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

// Configure the global Rayon thread pool with a large worker stack.
//
// Decompilation recurses deeply (CFG structure recovery, closure resolution)
// and runs across Rayon workers. Rayon's default worker stack (~2 MB) overflows
// and aborts the process on large real-world bundles (e.g. a Metro `global`
// function). Call this once at program start, before any decompilation, so
// every worker gets enough stack. It is idempotent and best-effort: if the pool
// is already initialized it does nothing.
pub fn configure_thread_pool() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let _ = rayon::ThreadPoolBuilder::new()
            .stack_size(64 * 1024 * 1024)
            .build_global();
    });
}

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
pub mod frida_hooks;
pub mod inspect;
pub mod ir;
pub mod secrets;
pub mod transforms;
/// Bytecode assemble / patch / serialize (see `ROADMAP.md`). Independent of decomp.
pub mod write;

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

pub use inspect::{
    dump_table, dump_table_json, function_info_banner, render_call_graph, TableKind,
};

pub use pipeline::{
    analyze_module, build_closure_context_from_file as build_closure_context,
    decompile_all_v2_with_closures, decompile_all_v2_with_closures_cached, decompile_filtered_v2,
    decompile_filtered_v2_cached, decompile_function_v2, decompile_function_v2_with_context,
    default_cache_path, generate_ir, progress_enabled, set_progress_enabled, DecompileOptionsV2,
    Decompiler, ModuleFilter, PipelineContext, CACHE_VERSION,
};

// Write-path surface.
pub use write::{
    assemble_function_hasm, assemble_module, create_minimal, emit_hasm_function,
    encode_function_body, encode_instruction, inject_stub, parse_hasm, parse_hasm_with_context,
    patch_function_body, patch_function_bytes, patch_string_by_id, patch_string_replace,
    serialize_file, verify_footer, CreateOptions, HasmModule, InjectStubKind, PatchOptions,
    SerializeOptions,
};

pub use secrets::{format_secrets_report, scan_secrets, scan_secrets_with_custom, SecretHit};
pub use frida_hooks::{
    build_metro_registry, generate_frida_for_file, generate_frida_hooks, list_modules_summary,
    FridaBundle, FridaHookOptions,
};
