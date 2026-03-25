mod batch;
mod context;
mod decompiler;
mod ir_gen;
mod stages;

pub use batch::{analyze_module, decompile_all_v2_with_closures};
pub use context::PipelineContext;
pub use decompiler::Decompiler;
pub use ir_gen::{build_closure_context_from_file, generate_ir};

use std::collections::{HashMap};
use crate::analysis::ClosureContext;
use crate::error::Result;
use crate::file::BytecodeFile;
use crate::opcode::BytecodeFormat;
use crate::transforms::{Codegen, CodegenOptions};
use crate::util::is_valid_identifier;

#[derive(Debug, Clone, Default)]
pub struct DecompileOptionsV2 {
    pub resolve_strings: bool,
    pub include_offsets: bool,
    pub propagate: bool,
    pub simplify: bool,
    pub recover_structures: bool,
    pub assembly_mode: bool,
}

impl DecompileOptionsV2 {
    pub fn optimized() -> Self {
        Self {
            resolve_strings: true,
            include_offsets: false,
            propagate: true,
            simplify: true,
            recover_structures: true,
            assembly_mode: false,
        }
    }

    pub fn debug() -> Self {
        Self {
            resolve_strings: true,
            include_offsets: true,
            propagate: false,
            simplify: false,
            recover_structures: true,
            assembly_mode: false,
        }
    }
}

pub fn decompile_function_v2(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    options: &DecompileOptionsV2,
) -> Result<String> {
    decompile_function_v2_with_context(file, format, function_id, options, None)
}

pub fn decompile_function_v2_with_context(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    options: &DecompileOptionsV2,
    closure_ctx: Option<&ClosureContext>,
) -> Result<String> {
    let statements = generate_ir(file, format, function_id, options, closure_ctx, true)?;

    let function_name = get_function_name(file, function_id);
    let params = get_function_params(file, function_id);

    let codegen_options = CodegenOptions::default();
    let mut codegen = Codegen::new(codegen_options);

    let mut output = String::new();
    output.push_str(&format!(
        "function {}({}) {{\n",
        function_name,
        params.join(", ")
    ));

    let body = codegen.generate_statements(&statements);
    for line in body.lines() {
        output.push_str("  ");
        output.push_str(line);
        output.push('\n');
    }

    output.push_str("}\n");
    Ok(output)
}

pub(crate) fn apply_register_naming(
    statements: Vec<crate::ir::Statement>,
    file: &BytecodeFile,
    function_id: u32,
) -> Vec<crate::ir::Statement> {
    use crate::analysis::{analyze_registers, generate_name, rename_registers};
    use std::collections::{BTreeMap, HashSet};

    let reg_info = analyze_registers(&statements);

    let debug_names: BTreeMap<u32, String> = if let Some(debug_info) = &file.debug_info {
        let scope_offset = debug_info
            .source_locations
            .get(&function_id)
            .and_then(|locs| locs.iter().find_map(|l| l.scope_offset));
        debug_info.build_variable_map(scope_offset)
    } else {
        BTreeMap::new()
    };

    let mut used_names = HashSet::new();
    for name in debug_names.values() {
        used_names.insert(name.clone());
    }

    let names: BTreeMap<u32, String> = reg_info
        .iter()
        .map(|(&r, info)| {
            if let Some(name) = debug_names.get(&r) {
                (r, name.clone())
            } else {
                (r, generate_name(info, &mut used_names))
            }
        })
        .collect();

    rename_registers(statements, &names)
}

fn get_function_name(file: &BytecodeFile, function_id: u32) -> String {
    file.function_headers
        .get(function_id as usize)
        .and_then(|h| file.string_at(h.function_name()))
        .filter(|e| !e.value.is_empty() && is_valid_identifier(&e.value))
        .map(|e| e.value.clone())
        .unwrap_or_else(|| format!("f{function_id}"))
}

fn get_function_params(file: &BytecodeFile, function_id: u32) -> Vec<String> {
    let param_count = file
        .function_headers
        .get(function_id as usize)
        .map(|h| h.param_count())
        .unwrap_or(0);

    (0..param_count).map(|i| format!("arg{i}")).collect()
}

fn build_function_name_index(file: &BytecodeFile) -> crate::analysis::FunctionNameIndex {
    let mut index = HashMap::new();

    for (id, header) in file.function_headers.iter().enumerate() {
        if let Some(entry) = file.string_at(header.function_name()) {
            let name = &entry.value;
            if !name.is_empty() && is_valid_identifier(name) {
                index
                    .entry(name.clone())
                    .or_insert_with(Vec::new)
                    .push(id as u32);
            }
        }
    }

    index
}
