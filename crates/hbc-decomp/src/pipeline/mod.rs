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
    /// Deep naming: iterate the naming stages to a fixed point and ground names in
    /// the bytecode data flow. Slower; recovers more real names and cuts noise.
    pub deep: bool,
    /// Stable output for build to build diffing: name otherwise-unnamed modules from
    /// a hash of their stable content instead of the volatile Metro id, so the same
    /// module keeps the same name across builds.
    pub stable: bool,
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
            deep: false,
            stable: false,
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
            deep: false,
            stable: false,
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
    let mut statements = generate_ir(file, format, function_id, options, closure_ctx, true)?;

    let function_name = get_function_name(file, function_id);
    let params = get_function_params(file, function_id);

    // The whole program pipeline runs several post generation stages that the
    // single function path skipped, so its output kept the receiver duplicated in
    // calls (`x.indexOf(x, y)`) and left every temporary in place (`tmp = arg0`).
    // Apply the same intra function cleanup here: strip the Hermes `this`
    // (single function has no IPA that needs the receiver slot), inline single use
    // temporaries, drop noise, then insert declarations.
    if options.simplify {
        crate::transforms::strip_hermes_this(&mut statements);
        statements = crate::transforms::inline_named_variables(statements);
        statements = crate::transforms::cleanup_noise(statements);
        crate::transforms::rename_reserved_words(&mut statements);
        crate::transforms::insert_declarations(&mut statements, &params);
    }

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

// How many arguments the function actually declares. `param_count` includes the
// implicit `this` (Hermes LoadParam index 0); the body names user arguments
// 0-indexed (LoadParam idx -> Parameter(idx-1) -> argN), so `this` is not one of
// them. This count is authoritative for a signature: recovered names may propose
// more entries than the function has (a callback role table suggests a name per
// conventional position, `item`/`index` for `.map`, `acc`/`item`/`index` for
// `.reduce`) and rendering those extras would invent parameters, changing the
// function's arity.
pub(crate) fn user_param_count(file: &BytecodeFile, function_id: u32) -> usize {
    file.function_headers
        .get(function_id as usize)
        .map(|h| h.param_count())
        .unwrap_or(0)
        .saturating_sub(1) as usize
}

// Build a signature from recovered names, cut to the arity the function really
// declares and padded with `argN` where no name was recovered.
//
// The header arity is a lower bound, not an exact count: Hermes does not count a
// default or rest parameter in it (`function greet(name = "anon")` reports zero
// declared arguments and reads the value in the body instead). So a recovered
// name past that bound is kept when the body actually reads that parameter, and
// dropped when it does not, which is the callback role case (`.map` proposes
// `item, index` even for a one argument callback).
pub(crate) fn params_from_names(
    file: &BytecodeFile,
    function_id: u32,
    names: &[Option<String>],
    body: &[crate::ir::Statement],
) -> Vec<String> {
    let declared = user_param_count(file, function_id);
    let mut count = declared;
    while count < names.len() && body_uses_param(body, count as u32) {
        count += 1;
    }
    (0..count)
        .map(|idx| {
            names
                .get(idx)
                .cloned()
                .flatten()
                .unwrap_or_else(|| format!("arg{idx}"))
        })
        .collect()
}

// Whether the body reads `Parameter(idx)`. Must run on the IR before parameter
// renaming, which rewrites those nodes into named variables.
fn body_uses_param(body: &[crate::ir::Statement], idx: u32) -> bool {
    use crate::ir::{Expression, Value, Visitor};
    struct V {
        idx: u32,
        found: bool,
    }
    impl<'a> Visitor<'a> for V {
        fn visit_expression(&mut self, e: &'a Expression) {
            if let Expression::Value(Value::Parameter(i)) = e {
                if *i == self.idx {
                    self.found = true;
                }
            }
            self.walk_expression(e);
        }
    }
    let mut v = V { idx, found: false };
    for s in body {
        v.visit_statement(s);
    }
    v.found
}

fn get_function_params(file: &BytecodeFile, function_id: u32) -> Vec<String> {
    (0..user_param_count(file, function_id))
        .map(|i| format!("arg{i}"))
        .collect()
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
