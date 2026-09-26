use super::esm_imports::{
    consolidate_imports, fold_redundant_imports, make_default_imports_distinct,
};
use super::{replace_whole_word, sanitize_import_name, Codegen, DescriptorInfo, EsmClassification};
use crate::ir::Statement;

impl Codegen {
    // Generate ESM-style module output from IR statements.
    // Classifies statements into imports, body, and exports at the IR level,
    // replacing regex-based text rewriting.
    pub fn generate_esm_module(
        &mut self,
        statements: &[Statement],
        module_id: u32,
        module_name: Option<&str>,
    ) -> String {
        use std::collections::{HashMap, HashSet};

        // Pre-pass 0: Detect re-export modules (Object.keys(source).forEach pattern)
        // These modules just re-export everything from another module.
        if let Some(reexport) = self.detect_reexport_module(statements) {
            let mut output = String::new();
            if let Some(name) = module_name {
                output.push_str(&format!("// Module {module_id} ({name})\n"));
            } else {
                output.push_str(&format!("// Module {module_id}\n"));
            }
            output.push_str(&reexport);
            output.push('\n');
            return output;
        }

        // Pre-pass: rename a generic import binding (`closure_3 = require(id)`) to
        // its module's name, so it renders as `import _slicedToArray from
        // "_slicedToArray"` instead of `import closure_3 from "_slicedToArray"`, and
        // several captures of one module collapse to a single binding (which import
        // consolidation then folds into one line).
        let import_renames = self.import_binding_renames(statements);
        let renamed_owned: Vec<Statement>;
        let statements: &[Statement] = if import_renames.is_empty() {
            statements
        } else {
            let mut s = statements.to_vec();
            crate::analysis::naming::rename_variables_in_stmts(&mut s, &import_renames);
            renamed_owned = s;
            &renamed_owned
        };

        // Pre-pass: names the module writes to more than once at top level. The
        // first write may look like a module load and become an import, but an ESM
        // import binding is immutable, so any later write to the same name has to
        // land on a local instead.
        // Counting reaches nested blocks and bodies, not just the top level, because
        // a write buried in a branch invalidates the import just as surely. Counting
        // one name too many only costs a local alias that is still correct JS, while
        // missing one emits an assignment to an import, which is not.
        let rebound: HashSet<String> = {
            use crate::ir::{AssignTarget, Visitor};
            struct W<'a>(&'a mut HashMap<String, u32>);
            impl<'b> Visitor<'b> for W<'_> {
                fn visit_statement(&mut self, s: &'b Statement) {
                    if let Statement::Let { name, .. } = s {
                        *self.0.entry(name.clone()).or_insert(0) += 1;
                    }
                    self.walk_statement(s);
                }
                fn visit_assign_target(&mut self, t: &'b AssignTarget) {
                    if let AssignTarget::Binding(crate::ir::Binding::Variable(name)) = t {
                        *self.0.entry(name.clone()).or_insert(0) += 1;
                    }
                    self.walk_assign_target(t);
                }
            }
            let mut writes: HashMap<String, u32> = HashMap::new();
            {
                let mut w = W(&mut writes);
                for stmt in statements {
                    w.visit_statement(stmt);
                }
            }
            // Bodies inlined into this module are rendered strings by now, so their
            // writes are folded in from the count the pipeline took over the IR of
            // every descendant function.
            for (name, count) in &self.nested_writes {
                *writes.entry(name.clone()).or_insert(0) += *count as u32;
            }
            writes
                .into_iter()
                .filter(|(_, c)| *c > 1)
                .map(|(n, _)| n)
                .collect()
        };

        // Pre-pass: collect descriptor variables (objects with get/value used in defineProperty)
        let mut descriptor_vars: HashMap<String, DescriptorInfo> = HashMap::new();
        let mut consumed_descriptors: HashSet<String> = HashSet::new();

        // Pass 1: Find all Let/Assign that define descriptor-like objects
        for stmt in statements {
            match stmt {
                Statement::Let { name, value, .. } => {
                    if let Some(info) = self.extract_descriptor_info(value) {
                        descriptor_vars.insert(name.clone(), info);
                    }
                }
                Statement::Assign {
                    target: crate::ir::AssignTarget::Binding(crate::ir::Binding::Variable(name)),
                    value,
                } => {
                    if let Some(info) = self.extract_descriptor_info(value) {
                        descriptor_vars.insert(name.clone(), info);
                    }
                }
                _ => {}
            }
        }

        // Pass 2: Find defineProperty calls that reference descriptor vars, mark them consumed
        for stmt in statements {
            if let Statement::Expr(expr) = stmt {
                if let Some(var_name) = self.get_define_property_descriptor_var(expr) {
                    if descriptor_vars.contains_key(&var_name) {
                        consumed_descriptors.insert(var_name);
                    }
                }
            }
        }

        // Pre-pass 3: Detect `Object.keys(X) + X.forEach(...)` re-export pairs
        // Pattern: X = Object.keys(X); let _ = X.forEach(cb) → export * from "modName"
        // Build a map of import variable → module name from the statements
        let mut import_var_to_module: HashMap<String, String> = HashMap::new();
        for stmt in statements {
            match stmt {
                Statement::Let { name, value, .. }
                | Statement::Assign {
                    target: crate::ir::AssignTarget::Binding(crate::ir::Binding::Variable(name)),
                    value,
                } => {
                    if let Some(mod_name) = self.resolve_require_module(value) {
                        import_var_to_module.insert(name.clone(), mod_name);
                    }
                    // Also check wrapper(require(N))
                    if let crate::ir::Expression::Call { arguments, .. } = value {
                        for arg in Self::effective_args(arguments) {
                            if let Some(mod_name) = self.resolve_require_module(arg) {
                                import_var_to_module.insert(name.clone(), mod_name);
                                break;
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Find Object.keys(X) assignments followed by X.forEach(...) calls
        let mut reexport_vars: HashSet<String> = HashSet::new(); // vars that are re-exported via forEach
        let mut reexport_skip_stmts: HashSet<usize> = HashSet::new(); // indices to skip
        let mut reexport_exports: Vec<(usize, String)> = Vec::new(); // (insert_at_index, export_line)

        for (i, stmt) in statements.iter().enumerate() {
            // Detect: X = Object.keys(X) (Assign where value is keys() call)
            // Also extract the source variable from Object.keys(SRC)
            let keys_info = match stmt {
                Statement::Assign {
                    target: crate::ir::AssignTarget::Binding(crate::ir::Binding::Variable(name)),
                    value,
                } => {
                    if self.is_object_keys_call(value) {
                        let src = self
                            .extract_object_keys_source(value)
                            .unwrap_or_else(|| name.clone());
                        Some((name.clone(), src))
                    } else {
                        None
                    }
                }
                Statement::Let { name, value, .. } => {
                    if self.is_object_keys_call(value) {
                        let src = self
                            .extract_object_keys_source(value)
                            .unwrap_or_else(|| name.clone());
                        Some((name.clone(), src))
                    } else {
                        None
                    }
                }
                _ => None,
            };
            if let Some((target_var, source_var)) = keys_info {
                // Look for the next statement: _ = X.forEach(callback) or let _ = X.forEach(callback)
                if i + 1 < statements.len() {
                    let next = &statements[i + 1];
                    let is_foreach = match next {
                        Statement::Let { value, .. } => self.is_foreach_on_var(value, &target_var),
                        Statement::Expr(value) => self.is_foreach_on_var(value, &target_var),
                        Statement::Assign { value, .. } => {
                            self.is_foreach_on_var(value, &target_var)
                        }
                        _ => false,
                    };
                    if is_foreach {
                        // Try source var first (Object.keys(source)), then target var
                        let mod_name = import_var_to_module
                            .get(&source_var)
                            .or_else(|| import_var_to_module.get(&target_var));
                        if let Some(mod_name) = mod_name {
                            reexport_skip_stmts.insert(i);
                            reexport_skip_stmts.insert(i + 1);
                            reexport_exports.push((i, format!("export * from \"{mod_name}\";")));
                            reexport_vars.insert(source_var);
                        }
                    }
                }
            }
        }

        let mut imports = Vec::new();
        let mut body_stmts = Vec::new();
        // Local aliases already declared for a re-bound module load.
        let mut declared_aliases: HashSet<String> = HashSet::new();
        let mut exports = Vec::new();

        for (i, stmt) in statements.iter().enumerate() {
            // Skip statements consumed by re-export pattern
            if reexport_skip_stmts.contains(&i) {
                // If this index has a re-export line, emit it
                for (idx, line) in &reexport_exports {
                    if *idx == i {
                        exports.push(line.clone());
                    }
                }
                continue;
            }

            // Skip Let/Assign that define consumed descriptor variables
            let skip_descriptor = match stmt {
                Statement::Let { name, .. } => consumed_descriptors.contains(name),
                Statement::Assign {
                    target: crate::ir::AssignTarget::Binding(crate::ir::Binding::Variable(name)),
                    ..
                } => consumed_descriptors.contains(name),
                _ => false,
            };
            if skip_descriptor {
                continue;
            }

            // Skip import statements for variables that became export * re-exports
            // (the import is subsumed by the export * from)
            let is_reexport_import = match stmt {
                Statement::Let { name, .. }
                | Statement::Assign {
                    target: crate::ir::AssignTarget::Binding(crate::ir::Binding::Variable(name)),
                    ..
                } => reexport_vars.contains(name),
                _ => false,
            };

            match self.classify_esm_stmt_with_descriptors(stmt, &descriptor_vars, &rebound) {
                EsmClassification::Import(line) => {
                    // Skip import for re-exported modules
                    if is_reexport_import {
                        continue;
                    }
                    imports.push(line);
                }
                EsmClassification::Export(line) => exports.push(line),
                EsmClassification::ImportAndExport(imp, exp) => {
                    imports.push(imp);
                    exports.push(exp);
                }
                EsmClassification::ImportAndBody(imp, line) => {
                    if !is_reexport_import {
                        imports.push(imp);
                    }
                    // A module can load the same dependency from several places, and
                    // each load asks for the same local alias. Declaring it every
                    // time redeclares the binding, which is a syntax error, so only
                    // the first one declares and the rest assign.
                    body_stmts.push(alias_line_once(line, &mut declared_aliases));
                }
                EsmClassification::Skip => {}
                EsmClassification::Body => body_stmts.push(self.generate_stmt(stmt)),
            }
        }

        // Post-pass: rename closure_N imports to meaningful names
        // e.g. `import closure_0 from "_typeof"` → `import _typeof from "_typeof"`
        let mut closure_renames: HashMap<String, String> = HashMap::new();
        let mut used_import_names: HashSet<String> = HashSet::new();
        // Collect names already used by non-closure imports
        for imp in &imports {
            // Extract import name from patterns like `import X from` or `import { X }` or `import { Y as X }`
            if let Some(rest) = imp.strip_prefix("import ") {
                if let Some(name) = rest.split_whitespace().next() {
                    if !name.starts_with('{') && !name.starts_with('*') {
                        used_import_names.insert(name.to_string());
                    }
                }
            }
        }
        for imp in &imports {
            // Match: import closure_N from "modName";
            if let Some(rest) = imp.strip_prefix("import ") {
                let parts: Vec<&str> = rest.splitn(3, ' ').collect();
                if parts.len() >= 3 && parts[0].starts_with("closure_") && parts[1] == "from" {
                    let mod_name = parts[2].trim_matches(|c| c == '"' || c == ';');
                    let sanitized = sanitize_import_name(mod_name);
                    if !sanitized.is_empty()
                        && sanitized != parts[0]
                        && !used_import_names.contains(&sanitized)
                    {
                        used_import_names.insert(sanitized.clone());
                        closure_renames.insert(parts[0].to_string(), sanitized);
                    }
                }
            }
            // Match: import { default as closure_N } from "modName";
            if imp.contains("default as closure_") {
                if let Some(start) = imp.find("default as closure_") {
                    let after = &imp[start + "default as ".len()..];
                    if let Some(end) = after.find([' ', '}']) {
                        let closure_name = &after[..end];
                        if closure_name.starts_with("closure_") {
                            if let Some(from_idx) = imp.find("from \"") {
                                let mod_part = &imp[from_idx + 6..];
                                if let Some(end_quote) = mod_part.find('"') {
                                    let mod_name = &mod_part[..end_quote];
                                    let sanitized = sanitize_import_name(mod_name);
                                    if !sanitized.is_empty()
                                        && sanitized != closure_name
                                        && !used_import_names.contains(&sanitized)
                                    {
                                        used_import_names.insert(sanitized.clone());
                                        closure_renames.insert(closure_name.to_string(), sanitized);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        // Apply renames to imports and body (using whole-word replacement to avoid partial matches)
        // Sort renames by key for deterministic output
        if !closure_renames.is_empty() {
            let mut sorted_renames: Vec<_> = closure_renames.iter().collect();
            sorted_renames.sort_by_key(|(a, _)| *a);
            for imp in imports.iter_mut() {
                for (old, new_name) in &sorted_renames {
                    *imp = replace_whole_word(imp, old, new_name);
                }
            }
            for body in body_stmts.iter_mut() {
                for (old, new_name) in &sorted_renames {
                    *body = replace_whole_word(body, old, new_name);
                }
            }
            for exp in exports.iter_mut() {
                for (old, new_name) in &sorted_renames {
                    *exp = replace_whole_word(exp, old, new_name);
                }
            }
        }

        // Consolidate imports: drop the repeated identical lines a module emits when
        // it requires the same dependency from many functions (e.g. `import _curry2
        // from "_curry2";` x65), and merge distinct named imports of the same module
        // into one `import { a, b } from "M";`.
        let mut imports = consolidate_imports(imports);
        // Consolidation collapses the same dependency imported twice. Two
        // different dependencies that inferred one name are still two lines
        // binding it, so they are separated here, body included.
        make_default_imports_distinct(&mut imports, &mut body_stmts, &mut exports);
        let (imports, mut extra_consts) =
            fold_redundant_imports(imports, &mut body_stmts, &mut exports);

        // Deduplicate exports (e.g. multiple export * from same module)
        {
            let mut seen = HashSet::new();
            exports.retain(|e| seen.insert(e.clone()));
        }
        // `module.exports = exports.default` at the end of a module re-exports
        // the default it already declared; a second `export default` is a parse
        // error and the whole module would be lost for it.
        if exports
            .iter()
            .filter(|e| e.starts_with("export default "))
            .count()
            > 1
        {
            exports.retain(|e| e.trim() != "export default exports.default;");
        }

        // `function name(){…}` + `export const name = …` → `export function name`
        dedupe_function_export_collisions(&mut body_stmts, &mut exports);
        demote_alias_lines_shadowed_by_declarations(&mut body_stmts);

        // `const X = ...` in the body plus `export const X = ...` binds X twice at
        // module level, which a parser rejects and the whole module is lost. The
        // exported value is a different expression from the local, so the local
        // cannot simply be promoted: the export takes a private binding and is
        // published under the name it always had.
        dedupe_const_export_collisions(&imports, &mut body_stmts, &mut exports);

        // A hoisted `let X;` in front of a `class X` or `function X` redeclares it,
        // which a parser rejects and takes the whole module down with it. The hoist
        // is inserted per function, before the class reconstruction has turned the
        // assignment into a declaration, so only the assembled module can see the
        // pair.
        drop_hoists_shadowed_by_declarations(&mut body_stmts);

        // A module that still calls `require` needs it bound. Metro keeps some
        // dependencies lazy, most visibly in the React Native index where every
        // export is a getter closing over `require(...)`, and turning those into
        // static imports would load eagerly and change what the module does. The
        // call is therefore kept as written, and the binding it needs is declared
        // here: Metro publishes its loader on the global as `__r`.
        // The binding and the call may sit in different lists (a `const
        // require = ...` in the body, the calls in the export lines), so both
        // are read as one module before deciding.
        let module_lines: Vec<String> = body_stmts.iter().chain(exports.iter()).cloned().collect();
        if body_calls_require(&module_lines) {
            extra_consts.insert(0, "const require = globalThis.__r;".to_string());
        }

        // Names the module writes without ever binding. Hermes shares one
        // environment slot between a function and the closures inside it, and the
        // pass that inserts declarations runs per function, so a slot owned by a
        // function that renders as an inline body is written by everyone and
        // declared by nobody. A module is always strict, so those writes throw
        // instead of quietly making a global. The module is the only scope that
        // sees every rendered body, so the binding is declared here.
        let dangling = undeclared_assignments(&imports, &body_stmts, &exports);
        if !dangling.is_empty() {
            extra_consts.push(format!("let {};", dangling.join(", ")));
        }

        // Build output
        let mut output = String::new();

        // Module header
        if let Some(name) = module_name {
            output.push_str(&format!("// Module {module_id} ({name})\n"));
        } else {
            output.push_str(&format!("// Module {module_id}\n"));
        }

        // Imports
        if !imports.is_empty() {
            for imp in &imports {
                output.push_str(imp);
                output.push('\n');
            }
            if extra_consts.is_empty() {
                output.push('\n');
            }
        }
        if !extra_consts.is_empty() {
            if !imports.is_empty() {
                output.push('\n');
            }
            for c in &extra_consts {
                output.push_str(c);
                output.push('\n');
            }
            output.push('\n');
        }

        // Body (skip leading/trailing empty lines)
        let body_text: String = body_stmts.concat();
        let trimmed = body_text.trim();
        if !trimmed.is_empty() {
            output.push_str(trimmed);
            output.push('\n');
        }

        // Exports
        if !exports.is_empty() {
            output.push('\n');
            for exp in &exports {
                output.push_str(exp);
                output.push('\n');
            }
        }

        output
    }
}

/// When body already has `function name(…)` and exports have `export const name = …`,
/// promote the declaration to `export function name` and drop the export const.
fn dedupe_function_export_collisions(body_stmts: &mut [String], exports: &mut Vec<String>) {
    use std::collections::HashSet;

    let mut fn_names: HashSet<String> = HashSet::new();
    for body in body_stmts.iter() {
        for line in body.lines() {
            let t = line.trim_start();
            if t.starts_with("export ") {
                continue;
            }
            let rest = if let Some(r) = t.strip_prefix("async function ") {
                r.trim_start_matches('*').trim_start()
            } else if let Some(r) = t.strip_prefix("function ") {
                r.trim_start_matches('*').trim_start()
            } else {
                continue;
            };
            if let Some(name) = rest.split(|c: char| c == '(' || c.is_whitespace()).next() {
                if !name.is_empty() && crate::util::is_valid_identifier(name) {
                    fn_names.insert(name.to_string());
                }
            }
        }
    }
    if fn_names.is_empty() {
        return;
    }

    let mut promote: HashSet<String> = HashSet::new();
    exports.retain(|exp| {
        let Some(rest) = exp.strip_prefix("export const ") else {
            return true;
        };
        let Some((name, _)) = rest.split_once(" = ") else {
            return true;
        };
        let name = name.trim();
        if fn_names.contains(name) {
            promote.insert(name.to_string());
            false
        } else {
            true
        }
    });
    if promote.is_empty() {
        return;
    }

    // Only a declaration at the top level of the module is the exported
    // one; the same name declared inside a nested body must stay as it is,
    // `export` is not allowed there.
    for body in body_stmts.iter_mut() {
        let mut lines: Vec<String> = body.lines().map(str::to_string).collect();
        let mut changed = false;
        for line in lines.iter_mut() {
            if line.starts_with("export ") || line.starts_with(char::is_whitespace) {
                continue;
            }
            for name in &promote {
                let heads = [
                    format!("async function* {name}("),
                    format!("async function {name}("),
                    format!("function* {name}("),
                    format!("function {name}("),
                ];
                if heads.iter().any(|h| line.starts_with(h.as_str())) {
                    line.insert_str(0, "export ");
                    changed = true;
                    break;
                }
            }
        }
        if changed {
            let mut rebuilt = lines.join("\n");
            if body.ends_with('\n') {
                rebuilt.push('\n');
            }
            *body = rebuilt;
        }
    }
}

impl Codegen {
    // Map each generic import binding (`closure_3 = require(id)`) to its module's
    // name so it renders as `import <module> from "<module>"`. Multiple captures of
    // the same module map to the same name (they hold the same value, so merging is
    // correct). A target that collides with an unrelated existing binding, or a
    // module whose inferred name is generic, is skipped.
    fn import_binding_renames(
        &self,
        statements: &[Statement],
    ) -> std::collections::BTreeMap<String, String> {
        use crate::ir::{AssignTarget, Expression, Value, Visitor};
        use std::collections::{BTreeMap, HashSet};

        // Every variable name referenced in the body (collision guard).
        let mut body_vars: HashSet<String> = HashSet::new();
        struct V<'a>(&'a mut HashSet<String>);
        impl<'a> Visitor<'a> for V<'_> {
            fn visit_expression(&mut self, e: &'a Expression) {
                if let Expression::Value(Value::Binding(crate::ir::Binding::Variable(n))) = e {
                    self.0.insert(n.clone());
                }
                self.walk_expression(e);
            }
            // A class the module declares binds its name as firmly as a
            // variable does; an import must not take it.
            fn visit_binding_def(&mut self, name: &'a str) {
                self.0.insert(name.to_string());
            }
        }
        {
            let mut v = V(&mut body_vars);
            for s in statements {
                v.visit_statement(s);
            }
        }

        let mut renames: BTreeMap<String, String> = BTreeMap::new();
        // One binding per distinct module id, so two captures of the SAME module
        // merge but two DIFFERENT modules that inferred the same name never collapse.
        let mut id_to_binding: std::collections::HashMap<u32, String> =
            std::collections::HashMap::new();
        let mut used_targets: HashSet<String> = HashSet::new();

        for stmt in statements {
            let (name, value) = match stmt {
                Statement::Let { name, value, .. } => (name, value),
                Statement::Assign {
                    target: AssignTarget::Binding(crate::ir::Binding::Variable(name)),
                    value,
                } => (name, value),
                _ => continue,
            };
            if !is_generic_import_binding(name) {
                continue;
            }
            // Resolve (module name, absolute id) from `require(id)` or
            // `wrapper(require(id))`.
            let resolved = self.resolve_require_module_id(value).or_else(|| {
                if let Expression::Call { arguments, .. } = value {
                    Self::effective_args(arguments)
                        .iter()
                        .find_map(|a| self.resolve_require_module_id(a))
                } else {
                    None
                }
            });
            let Some((mod_name, id)) = resolved else {
                continue;
            };
            let base = super::sanitize_import_name(&mod_name);
            if !crate::util::is_valid_identifier(&base) || is_bad_module_binding(&base) {
                continue;
            }

            let target = if let Some(existing) = id_to_binding.get(&id) {
                existing.clone()
            } else {
                // Uniquify against other assigned bindings and any unrelated body
                // variable of the same name (sources are `closure_N`, never a module
                // name, so a base colliding with body_vars is a genuine other var).
                let mut cand = base.clone();
                let mut i = 2u32;
                while used_targets.contains(&cand) || body_vars.contains(&cand) {
                    cand = format!("{base}{i}");
                    i += 1;
                }
                used_targets.insert(cand.clone());
                id_to_binding.insert(id, cand.clone());
                cand
            };
            renames.insert(name.clone(), target);
        }
        renames
    }
}

// A binding whose name is a decompiler placeholder that should take its module's
// name when it is an import.
fn is_generic_import_binding(name: &str) -> bool {
    if name.starts_with("closure_") {
        return true;
    }
    let result_stem = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if result_stem.ends_with("Result") {
        return true;
    }
    let digit_suffix = |prefix: &str| {
        name.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.chars().all(|c| c.is_ascii_digit()))
    };
    digit_suffix("tmp")
        || (name.starts_with('r')
            && name.len() > 1
            && name[1..].chars().all(|c| c.is_ascii_digit()))
        || digit_suffix("obj")
        || digit_suffix("arr")
}

// Module names too generic to become a binding (would not read better than the
// placeholder).
fn is_bad_module_binding(name: &str) -> bool {
    crate::analysis::metro::is_generic_module_specifier(name)
        || matches!(name, "module" | "exports" | "default" | "require")
}

// Whether the rendered lines call `require` without binding it first. A property
// access (`x.require(...)`) or a longer identifier ending in `require` is not a
// call to the module loader.
fn body_calls_require(lines: &[String]) -> bool {
    let mut calls = false;
    for line in lines {
        for (idx, _) in line.match_indices("require") {
            let before = line[..idx].chars().next_back();
            if matches!(before, Some(c) if c == '.' || c == '_' || c.is_alphanumeric()) {
                continue;
            }
            let after = line[idx + "require".len()..].trim_start();
            if after.starts_with('(') {
                calls = true;
            }
            // `const require = ...`, `let require = ...`, `require = ...` or a
            // parameter list entry all bind the name, so nothing is dangling.
            if after.starts_with('=') && !after.starts_with("==") {
                return false;
            }
        }
    }
    calls
}

// Every name the rendered module assigns to without binding it anywhere.
//
// Deliberately one sided: a name is reported only when no binding form for it
// appears anywhere in the module, so a missed binding form costs a redundant
// declaration rather than a wrong one. Builtin globals are never reported, since
// writing to one is the module's own business.
fn undeclared_assignments(imports: &[String], body: &[String], exports: &[String]) -> Vec<String> {
    use std::collections::HashSet;
    use std::sync::OnceLock;
    static PATS: OnceLock<(Vec<regex::Regex>, regex::Regex, regex::Regex)> = OnceLock::new();
    let (binders, assign, method) = PATS.get_or_init(|| {
        let binders = [
            // let / const / var, including a comma list and a destructuring head
            r"\b(?:let|const|var)\s+([A-Za-z_$][\w$]*(?:\s*,\s*[A-Za-z_$][\w$]*)*)",
            r"\b(?:let|const|var)\s*[\[{]([^\]}]*)[\]}]",
            // function name and parameter list, arrow parameters, catch binding
            r"\bfunction\s*\*?\s*([A-Za-z_$][\w$]*)?\s*\(([^)]*)\)",
            r"\(([^)]*)\)\s*=>",
            r"(?:^|[^.\w$])([A-Za-z_$][\w$]*)\s*=>",
            r"\bcatch\s*\(\s*([A-Za-z_$][\w$]*)",
            r"\bclass\s+([A-Za-z_$][\w$]*)",
            // import default and named list
            r"^\s*import\s+(?:\{([^}]*)\}|([A-Za-z_$][\w$]*))",
        ]
        .iter()
        .map(|p| regex::Regex::new(p).expect("static pattern"))
        .collect::<Vec<_>>();
        let assign = regex::Regex::new(
            r"^\s*([A-Za-z_$][\w$]*)\s*(?:=[^=>]|\+=|-=|\*=|/=|%=|&=|\^=|\|=|\+\+|--)",
        )
        .expect("static pattern");
        // A method shorthand head binds its parameters, but `if (x === 2) {` has
        // the very same shape, so the head word is checked against the statement
        // keywords before its parentheses are read as a parameter list.
        let method = regex::Regex::new(r"^\s*([A-Za-z_$][\w$]*)\s*\(([^)]*)\)\s*\{")
            .expect("static pattern");
        (binders, assign, method)
    });

    let mut bound: HashSet<&str> = HashSet::new();
    let mut assigned: Vec<&str> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();

    // Each rendered statement can span many lines, and the anchored patterns match
    // the start of the haystack, so the scan has to see one real line at a time.
    for chunk in imports.iter().chain(body).chain(exports) {
        for line in chunk.lines() {
            for rx in binders {
                for caps in rx.captures_iter(line) {
                    for group in caps.iter().skip(1).flatten() {
                        for part in group.as_str().split([',', ' ', '\t']) {
                            let name = part.trim().trim_start_matches("...");
                            // `a as b` binds b, `a = 1` binds a
                            let name = name.rsplit(" as ").next().unwrap_or(name);
                            let name = name.split('=').next().unwrap_or(name).trim();
                            if !name.is_empty() {
                                bound.insert(name);
                            }
                        }
                    }
                }
            }
            if let Some(caps) = method.captures(line) {
                let head = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                let is_keyword = matches!(
                    head,
                    "if" | "while"
                        | "for"
                        | "switch"
                        | "catch"
                        | "do"
                        | "else"
                        | "try"
                        | "return"
                        | "with"
                        | "function"
                        | "typeof"
                        | "in"
                        | "of"
                        | "new"
                );
                if !is_keyword {
                    if let Some(params) = caps.get(2) {
                        for part in params.as_str().split([',', ' ', '\t']) {
                            let name = part.trim().trim_start_matches("...");
                            let name = name.split('=').next().unwrap_or(name).trim();
                            if !name.is_empty() {
                                bound.insert(name);
                            }
                        }
                    }
                }
            }
            if let Some(caps) = assign.captures(line) {
                if let Some(m) = caps.get(1) {
                    if seen.insert(m.as_str()) {
                        assigned.push(m.as_str());
                    }
                }
            }
        }
    }

    let mut out: Vec<String> = assigned
        .into_iter()
        .filter(|n| {
            !bound.contains(n)
                && !crate::ir::expr::display::is_builtin_global(n)
                && crate::util::is_valid_identifier(n)
        })
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod const_export_tests {
    use super::dedupe_const_export_collisions;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn an_export_colliding_with_a_local_takes_a_private_binding() {
        // `const X` plus `export const X` binds X twice at module level, and the
        // module stops parsing. The exported value reads the very local it
        // collides with, so promoting the local would change what is exported.
        let mut body = v(&["const StackToolbar = load();\n"]);
        let mut exports = v(&["export const StackToolbar = StackToolbar.default;"]);
        dedupe_const_export_collisions(&[], &mut body, &mut exports);
        assert_eq!(
            exports,
            v(&["export { StackToolbar_export as StackToolbar };"])
        );
        assert_eq!(
            body[1],
            "const StackToolbar_export = StackToolbar.default;\n"
        );
    }

    // An interop unwrap comes out as `import X from "X"` then
    // `export const X = X.X`, which binds X twice at module level and loses the
    // whole module. The import binds the name just as a local const does.
    #[test]
    fn an_export_colliding_with_an_import_takes_a_private_binding() {
        let imports = v(&["import BasicAlertDialog from \"BasicAlertDialog\";"]);
        let mut body = v(&[]);
        let mut exports =
            v(&["export const BasicAlertDialog = BasicAlertDialog.BasicAlertDialog;"]);
        dedupe_const_export_collisions(&imports, &mut body, &mut exports);
        assert_eq!(
            exports,
            v(&["export { BasicAlertDialog_export as BasicAlertDialog };"])
        );
        assert_eq!(
            body[0],
            "const BasicAlertDialog_export = BasicAlertDialog.BasicAlertDialog;\n"
        );
    }

    #[test]
    fn a_named_import_specifier_binds_its_local_name() {
        assert_eq!(
            super::import_bound_names("import a, { b, c as d } from \"m\";"),
            vec!["a".to_string(), "b".to_string(), "d".to_string()]
        );
        assert_eq!(
            super::import_bound_names("import * as ns from \"m\";"),
            vec!["ns".to_string()]
        );
        assert!(super::import_bound_names("const x = 1;").is_empty());
    }

    #[test]
    fn an_export_with_no_local_of_that_name_is_untouched() {
        let mut body = v(&["const other = 1;\n"]);
        let before = v(&["export const StackToolbar = load();"]);
        let mut exports = before.clone();
        dedupe_const_export_collisions(&[], &mut body, &mut exports);
        assert_eq!(exports, before);
        assert_eq!(body.len(), 1, "nothing should be appended");
    }

    #[test]
    fn a_taken_alias_is_skipped() {
        let mut body = v(&["const X = 1;\n", "const X_export = 2;\n"]);
        let mut exports = v(&["export const X = X.default;"]);
        dedupe_const_export_collisions(&[], &mut body, &mut exports);
        assert_eq!(exports, v(&["export { X_export2 as X };"]));
    }

    #[test]
    fn a_declaration_already_exported_is_not_counted_as_a_local() {
        // `export const X` in the body is the export itself, not a competing local.
        let mut body = v(&["export const X = 1;\n"]);
        let before = v(&["export const X = load();"]);
        let mut exports = before.clone();
        dedupe_const_export_collisions(&[], &mut body, &mut exports);
        assert_eq!(exports, before);
    }
}

#[cfg(test)]
mod hoist_tests {
    use super::drop_hoists_shadowed_by_declarations;

    fn run(v: &[&str]) -> Vec<String> {
        let mut b: Vec<String> = v.iter().map(|x| (*x).to_string()).collect();
        drop_hoists_shadowed_by_declarations(&mut b);
        b
    }

    #[test]
    fn a_hoist_in_front_of_a_class_is_dropped() {
        // `let DOMRect;` then `class DOMRect ...` binds the name twice, which a
        // parser rejects outright and the whole module stops parsing.
        let out = run(&[
            "let DOMRect;",
            "class DOMRect extends Base {
}",
        ]);
        assert_eq!(out[0], "");
        assert!(out[1].starts_with("class DOMRect"));
    }

    #[test]
    fn a_hoist_in_front_of_a_function_is_dropped() {
        for decl in [
            "function handler(a) {
}",
            "export function handler(a) {
}",
            "function* handler(a) {
}",
        ] {
            let out = run(&["let handler;", decl]);
            assert_eq!(out[0], "", "hoist should go for {decl}");
        }
    }

    #[test]
    fn a_hoist_with_no_matching_declaration_stays() {
        let out = run(&["let counter;", "counter = 1;"]);
        assert_eq!(out[0], "let counter;");
    }

    #[test]
    fn a_hoist_of_a_different_name_stays() {
        let out = run(&[
            "let other;",
            "class DOMRect extends Base {
}",
        ]);
        assert_eq!(out[0], "let other;");
    }

    #[test]
    fn an_initialised_declaration_is_never_touched() {
        // Only a bare hoist is redundant. `let x = 1;` carries a value.
        let out = run(&[
            "let DOMRect = 1;",
            "class DOMRect extends Base {
}",
        ]);
        assert_eq!(out[0], "let DOMRect = 1;");
    }

    #[test]
    fn a_nested_hoist_inside_a_body_is_removed_only_when_it_matches() {
        // The hoist can be one line of a larger rendered chunk.
        let out = run(&[
            "let DOMRect;
const p = DOMRect.prototype;",
            "class DOMRect {
}",
        ]);
        assert_eq!(out[0], "const p = DOMRect.prototype;");
    }
}

#[cfg(test)]
mod alias_tests {
    use super::alias_line_once;
    use std::collections::HashSet;

    #[test]
    fn the_first_load_declares_and_the_rest_assign() {
        // A module loading the same dependency twice asked for the same alias
        // twice, and two `let size = size_mod;` lines are a syntax error that
        // stops the whole module from parsing.
        let mut seen = HashSet::new();
        let line = "let size = size_mod;\n".to_string();
        assert_eq!(
            alias_line_once(line.clone(), &mut seen),
            "let size = size_mod;\n"
        );
        assert_eq!(
            alias_line_once(line.clone(), &mut seen),
            "size = size_mod;\n"
        );
        assert_eq!(alias_line_once(line, &mut seen), "size = size_mod;\n");
    }

    #[test]
    fn two_different_aliases_each_keep_their_declaration() {
        let mut seen = HashSet::new();
        assert_eq!(
            alias_line_once("let a = a_mod;\n".to_string(), &mut seen),
            "let a = a_mod;\n"
        );
        assert_eq!(
            alias_line_once("let b = b_mod;\n".to_string(), &mut seen),
            "let b = b_mod;\n"
        );
    }

    #[test]
    fn a_line_that_is_not_a_declaration_is_passed_through() {
        let mut seen = HashSet::new();
        let line = "size = size_mod;\n".to_string();
        assert_eq!(alias_line_once(line.clone(), &mut seen), line);
        assert_eq!(alias_line_once(line.clone(), &mut seen), line);
    }
}

#[cfg(test)]
mod undeclared_tests {
    use super::undeclared_assignments;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn a_name_written_but_never_bound_is_reported() {
        let body = s(&["closure_0 = arg0;\nclosure_1 = arguments;"]);
        assert_eq!(
            undeclared_assignments(&[], &body, &[]),
            vec!["closure_0".to_string(), "closure_1".to_string()]
        );
    }

    #[test]
    fn every_binding_form_counts_as_bound() {
        for bound in [
            "let a;",
            "const a = 1;",
            "var a;",
            "function f(a) {}",
            "const g = (a) => a;",
            "const h = a => a;",
            "try {} catch (a) {}",
            "class a {}",
            "import a from \"m\";",
            "import { a } from \"m\";",
            "let x, a, y;",
        ] {
            let body = s(&[bound, "a = 1;"]);
            assert!(
                undeclared_assignments(&[], &body, &[]).is_empty(),
                "{bound} should bind `a`"
            );
        }
    }

    #[test]
    fn writing_to_a_builtin_global_is_the_modules_own_business() {
        let body = s(&["console = 1;", "Object = 2;"]);
        assert!(undeclared_assignments(&[], &body, &[]).is_empty());
    }

    #[test]
    fn a_write_nested_inside_a_rendered_statement_is_seen() {
        // A rendered statement spans many lines, so the scan has to look at each
        // one rather than only the start of the chunk.
        let body = s(&["function outer() {\n  deep = 1;\n}"]);
        assert_eq!(
            undeclared_assignments(&[], &body, &[]),
            vec!["deep".to_string()]
        );
    }

    #[test]
    fn a_statement_head_is_not_a_method_binding_its_parameters() {
        // `if (c3 === 2) {` has the shape of a method shorthand head, so its
        // condition was read as a parameter list and every name in it counted as
        // bound. That alone hid 7746 unbound writes on the reference bundle.
        for head in [
            "if (c3 === 2) {",
            "while (c3 < 2) {",
            "switch (c3) {",
            "for (c3 = 0; c3 < 2; c3++) {",
        ] {
            let body = s(&[head, "c3 = 3;"]);
            assert_eq!(
                undeclared_assignments(&[], &body, &[]),
                vec!["c3".to_string()],
                "{head} must not bind c3"
            );
        }
    }

    #[test]
    fn a_real_method_shorthand_still_binds_its_parameters() {
        let body = s(&["render(item) {\n  item = 1;\n}"]);
        assert!(undeclared_assignments(&[], &body, &[]).is_empty());
    }

    #[test]
    fn comparisons_and_arrows_are_not_assignments() {
        let body = s(&["if (a === 1) {}", "const f = a => a;", "b == 2;"]);
        assert!(undeclared_assignments(&[], &body, &[]).is_empty());
    }
}

// The local alias for a re-bound module load, declared once per module.
//
// A module can load the same dependency from several places, and every load
// asks for the same alias. Declaring it each time redeclares the binding, which
// a parser rejects outright, so the first occurrence declares and the rest
// assign to what it declared.
fn alias_line_once(line: String, declared: &mut std::collections::HashSet<String>) -> String {
    if declared.insert(line.clone()) {
        return line;
    }
    match line.strip_prefix("let ") {
        Some(rest) => rest.to_string(),
        None => line,
    }
}

// A rebound import leaves `let X = X_mod;` in the body. When the module also
// declares `X` at the top level in another form (a function, a class, a load
// through an alias the import pass did not recognise), the alias line is a
// second binding of the name; it becomes a plain assignment instead.
fn demote_alias_lines_shadowed_by_declarations(body: &mut [String]) {
    use std::collections::HashMap;
    let mut declared: HashMap<String, usize> = HashMap::new();
    let mut bound: HashMap<String, usize> = HashMap::new();
    let mut alias_lines: Vec<(usize, usize, String, Option<String>)> = Vec::new();
    for (ci, chunk) in body.iter().enumerate() {
        for (li, line) in chunk.lines().enumerate() {
            let rest = line.strip_prefix("export ").unwrap_or(line);
            let name = if let Some(r) = rest
                .strip_prefix("let ")
                .or_else(|| rest.strip_prefix("const "))
            {
                r.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
                    .next()
            } else if let Some(r) = rest.strip_prefix("class ") {
                r.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
                    .next()
            } else if let Some(r) = rest.strip_prefix("function") {
                r.trim_start()
                    .strip_prefix('*')
                    .unwrap_or(r)
                    .trim_start()
                    .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
                    .next()
            } else {
                None
            };
            let Some(name) = name.filter(|n| !n.is_empty()) else {
                continue;
            };
            // An import alias (`let X = X_mod;`), or any initialised `let`/`const`
            // of a name a class or function declaration also binds: hermesc's
            // inliner can reuse one environment slot for a value and, later, for
            // the class stored into it (`const Decimal = obj;` next to
            // `class Decimal`). The declaration owns the name; the line becomes
            // an assignment to it.
            let is_alias = rest.starts_with("let ")
                && rest.trim_end().ends_with("_mod;")
                && rest.trim_end() == format!("let {name} = {name}_mod;");
            let initialised = !line.starts_with("export ")
                && (rest.starts_with("let ") || rest.starts_with("const "))
                && rest[rest.find(name).unwrap_or(0) + name.len()..]
                    .trim_start()
                    .starts_with("= ");
            if is_alias {
                alias_lines.push((ci, li, name.to_string(), None));
            } else if initialised {
                alias_lines.push((ci, li, name.to_string(), Some(line.to_string())));
                *bound.entry(name.to_string()).or_insert(0) += 1;
            } else {
                *declared.entry(name.to_string()).or_insert(0) += 1;
                *bound.entry(name.to_string()).or_insert(0) += 1;
            }
        }
    }
    for (ci, li, name, full) in alias_lines {
        // An alias yields to any other binding of the name; an initialised
        // declaration only to a class, function or hoisted `let`.
        let shadowed = match &full {
            None => bound.contains_key(&name),
            Some(_) => declared.contains_key(&name),
        };
        if !shadowed {
            continue;
        }
        let chunk = &mut body[ci];
        let mut lines: Vec<String> = chunk.lines().map(str::to_string).collect();
        lines[li] = match full {
            None => format!("{name} = {name}_mod;"),
            Some(line) => {
                let rest = line
                    .strip_prefix("let ")
                    .or_else(|| line.strip_prefix("const "))
                    .unwrap_or(&line);
                rest.to_string()
            }
        };
        let mut rebuilt = lines.join("\n");
        if chunk.ends_with('\n') {
            rebuilt.push('\n');
        }
        *chunk = rebuilt;
    }
}

// Remove a top level `let X;` when the module also declares `X` as a class, a
// function or an initialised `let`/`const` at that level. Both bind the same name in the same scope, and two
// bindings of one name is a syntax error, so the hoist is the one to go: the
// declaration it was reserving a slot for arrived in a stronger form.
//
// Only top level lines count. A `let X;` nested inside a function is a different
// binding and shadowing there is legal.
fn drop_hoists_shadowed_by_declarations(body: &mut [String]) {
    use std::collections::HashSet;
    let mut declared: HashSet<&str> = HashSet::new();
    for chunk in body.iter() {
        for line in chunk.lines() {
            let rest = line.strip_prefix("export ").unwrap_or(line);
            // A `let X = ...` at the top level is the alias a rebound import
            // leaves behind; it binds X as surely as a class does, and the bare
            // hoist next to it is the redundant one.
            let initialised = rest
                .strip_prefix("let ")
                .or_else(|| rest.strip_prefix("const "))
                .filter(|r| r.contains('='));
            let rest = match rest.strip_prefix("class ") {
                Some(r) => r,
                None => match rest.strip_prefix("function") {
                    // `function name`, `function* name` and `function *name`
                    Some(r) => r.trim_start().strip_prefix('*').unwrap_or(r).trim_start(),
                    None => match initialised {
                        Some(r) => r,
                        None => continue,
                    },
                },
            };
            let name: &str = rest
                .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
                .next()
                .unwrap_or("");
            if !name.is_empty() {
                declared.insert(name);
            }
        }
    }
    if declared.is_empty() {
        return;
    }
    let shadowed: HashSet<String> = declared.iter().map(|n| format!("let {n};")).collect();
    for chunk in body.iter_mut() {
        if chunk.lines().any(|l| shadowed.contains(l.trim_end())) {
            let kept: Vec<&str> = chunk
                .lines()
                .filter(|l| !shadowed.contains(l.trim_end()))
                .collect();
            let mut rebuilt = kept.join("\n");
            if chunk.ends_with('\n') && !rebuilt.is_empty() {
                rebuilt.push('\n');
            }
            *chunk = rebuilt;
        }
    }
}

// Resolve `const X` in the body colliding with `export const X`.
//
// Unlike a function, whose declaration can simply be promoted, the exported value
// here is an expression of its own, often reading the very local it collides with
// (`export const X = X.default`). The value therefore moves into a private
// binding, and the export publishes that binding under the original name.
// `const X = ...` in the body, or `import X from ...`, plus `export const X = ...`
// binds X twice at module level and a parser rejects the whole module. The export
// takes a private binding and is published under the name it always had.
//
// The import side matters as much as the body side: an interop unwrap comes out as
// `import X from "X"` followed by `export const X = X.X`, and that shape alone
// accounted for 40 of the modules that would not parse.
// The names an import line binds: the default, the namespace, and each specifier
// under its local name when it is aliased.
fn import_bound_names(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let t = line.trim();
    let Some(rest) = t.strip_prefix("import ") else {
        return out;
    };
    let Some(head) = rest.split(" from ").next() else {
        return out;
    };
    let head = head.trim();
    let mut push = |name: &str| {
        let name = name.trim();
        let local = name.rsplit(" as ").next().unwrap_or(name).trim();
        if crate::util::is_valid_identifier(local) {
            out.push(local.to_string());
        }
    };
    if let Some(ns) = head.strip_prefix("* as ") {
        push(ns);
        return out;
    }
    // `X`, `{ a, b as c }`, or `X, { a }`.
    let (default_part, braced) = match head.find('{') {
        Some(i) => (&head[..i], Some(&head[i..])),
        None => (head, None),
    };
    for part in default_part.split(',') {
        if !part.trim().is_empty() {
            push(part);
        }
    }
    if let Some(braced) = braced {
        let inner = braced.trim_start_matches('{').trim_end_matches('}');
        for spec in inner.split(',') {
            if !spec.trim().is_empty() {
                push(spec);
            }
        }
    }
    out
}

fn dedupe_const_export_collisions(
    imports: &[String],
    body_stmts: &mut Vec<String>,
    exports: &mut [String],
) {
    use std::collections::HashSet;

    let mut declared: HashSet<String> = HashSet::new();
    for line in imports {
        for name in import_bound_names(line) {
            declared.insert(name);
        }
    }
    for body in body_stmts.iter() {
        for line in body.lines() {
            let t = line.trim_start();
            if t.starts_with("export ") {
                continue;
            }
            // A class or function declaration binds its name as surely as a
            // `const`; `class X {}` next to `export const X = _createClass(X)`
            // was the largest family of modules that would not parse.
            let rest = [
                "const ",
                "let ",
                "var ",
                "class ",
                "async function ",
                "function ",
            ]
            .iter()
            .find_map(|kw| t.strip_prefix(*kw))
            .map(|r| r.trim_start_matches('*').trim_start());
            let Some(rest) = rest else { continue };
            if let Some(name) = rest
                .split(|c: char| c == '=' || c == ';' || c == '(' || c == '{' || c.is_whitespace())
                .next()
            {
                if !name.is_empty() && crate::util::is_valid_identifier(name) {
                    declared.insert(name.to_string());
                }
            }
        }
    }
    if declared.is_empty() {
        return;
    }

    let mut extra: Vec<String> = Vec::new();
    for exp in exports.iter_mut() {
        let Some(rest) = exp.strip_prefix("export const ") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(" = ") else {
            continue;
        };
        let name = name.trim().to_string();
        if !declared.contains(&name) {
            continue;
        }
        let value = value.trim_end().trim_end_matches(';');
        let mut alias = format!("{name}_export");
        let mut n = 2u32;
        while declared.contains(&alias) {
            alias = format!("{name}_export{n}");
            n += 1;
        }
        declared.insert(alias.clone());
        extra.push(format!("const {alias} = {value};\n"));
        *exp = format!("export {{ {alias} as {name} }};");
    }
    body_stmts.extend(extra);
}
