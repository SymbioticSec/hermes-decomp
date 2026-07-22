mod batch;
mod cache;
mod context;
mod decompiler;
mod ir_gen;
mod progress;
mod stages;

pub use batch::{
    analyze_module, decompile_all_v2_with_closures, decompile_all_v2_with_closures_cached,
    decompile_filtered_v2, decompile_filtered_v2_cached, ModuleFilter,
};
pub use cache::{default_cache_path, CACHE_VERSION};
pub use context::PipelineContext;
pub use decompiler::Decompiler;
pub use ir_gen::{build_closure_context_from_file, generate_ir};
pub use progress::{is_enabled as progress_enabled, set_enabled as set_progress_enabled, status as progress_status};

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
    // Reserve every source-level variable name already present (destructuring
    // targets/keys, earlier-named variables, params). Generated register names
    // must not collide with them, otherwise two distinct bindings end up sharing
    // a name (`let {x, y}` clashing with a register also named `x`).
    collect_existing_var_names(&statements, &mut used_names);

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

// Collect every source-level variable name appearing in `statements` (variable
// values/targets and destructuring-pattern names), so register naming can reserve
// them and avoid collisions.
fn collect_existing_var_names(
    statements: &[crate::ir::Statement],
    out: &mut std::collections::HashSet<String>,
) {
    use crate::ir::{AssignTarget, Expression, Value, Visitor};
    struct C<'a>(&'a mut std::collections::HashSet<String>);
    impl<'a, 'b> Visitor<'b> for C<'a> {
        fn visit_expression(&mut self, e: &'b Expression) {
            if let Expression::Value(Value::Variable(n)) = e {
                self.0.insert(n.clone());
            }
            self.walk_expression(e);
        }
        fn visit_assign_target(&mut self, t: &'b AssignTarget) {
            collect_target_names(t, self.0);
            self.walk_assign_target(t);
        }
    }
    fn collect_target_names(t: &AssignTarget, out: &mut std::collections::HashSet<String>) {
        match t {
            AssignTarget::Variable(n) => {
                out.insert(n.clone());
            }
            AssignTarget::DestructuringArray(elems) => {
                for e in elems.iter().flatten() {
                    collect_target_names(&e.0, out);
                }
            }
            AssignTarget::DestructuringArrayRest { elements, rest } => {
                for e in elements.iter().flatten() {
                    collect_target_names(&e.0, out);
                }
                collect_target_names(rest, out);
            }
            AssignTarget::DestructuringObject(props) => {
                for p in props {
                    collect_target_names(&p.1, out);
                }
            }
            AssignTarget::DestructuringObjectRest { properties, rest } => {
                for p in properties {
                    collect_target_names(&p.1, out);
                }
                collect_target_names(rest, out);
            }
            _ => {}
        }
    }
    let mut c = C(out);
    for s in statements {
        c.visit_statement(s);
    }
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

    // param_count includes the implicit `this` (Hermes LoadParam index 0). The
    // body names user arguments 0-indexed (LoadParam idx -> Parameter(idx-1) ->
    // argN), so the signature must list the user args the same way and NOT show
    // `this`. Otherwise the signature's argN is the body's arg(N-1) (off by one).
    let user_params = param_count.saturating_sub(1);
    (0..user_params).map(|i| format!("arg{i}")).collect()
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
