use super::{Codegen, DescriptorInfo, EsmClassification, sanitize_import_name, replace_whole_word};
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
                Statement::Assign { target: crate::ir::AssignTarget::Variable(name), value } => {
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
                Statement::Let { name, value, .. } | Statement::Assign { target: crate::ir::AssignTarget::Variable(name), value } => {
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
        let mut reexport_vars: HashSet<String> = HashSet::new();  // vars that are re-exported via forEach
        let mut reexport_skip_stmts: HashSet<usize> = HashSet::new(); // indices to skip
        let mut reexport_exports: Vec<(usize, String)> = Vec::new(); // (insert_at_index, export_line)

        for (i, stmt) in statements.iter().enumerate() {
            // Detect: X = Object.keys(X) (Assign where value is keys() call)
            // Also extract the source variable from Object.keys(SRC)
            let keys_info = match stmt {
                Statement::Assign { target: crate::ir::AssignTarget::Variable(name), value } => {
                    if self.is_object_keys_call(value) {
                        let src = self.extract_object_keys_source(value)
                            .unwrap_or_else(|| name.clone());
                        Some((name.clone(), src))
                    } else { None }
                }
                Statement::Let { name, value, .. } => {
                    if self.is_object_keys_call(value) {
                        let src = self.extract_object_keys_source(value)
                            .unwrap_or_else(|| name.clone());
                        Some((name.clone(), src))
                    } else { None }
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
                        Statement::Assign { value, .. } => self.is_foreach_on_var(value, &target_var),
                        _ => false,
                    };
                    if is_foreach {
                        // Try source var first (Object.keys(source)), then target var
                        let mod_name = import_var_to_module.get(&source_var)
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
                Statement::Assign { target: crate::ir::AssignTarget::Variable(name), .. } => {
                    consumed_descriptors.contains(name)
                }
                _ => false,
            };
            if skip_descriptor {
                continue;
            }

            // Skip import statements for variables that became export * re-exports
            // (the import is subsumed by the export * from)
            let is_reexport_import = match stmt {
                Statement::Let { name, .. } | Statement::Assign { target: crate::ir::AssignTarget::Variable(name), .. } => {
                    reexport_vars.contains(name)
                }
                _ => false,
            };

            match self.classify_esm_stmt_with_descriptors(stmt, &descriptor_vars) {
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
                    if !sanitized.is_empty() && sanitized != parts[0] && !used_import_names.contains(&sanitized) {
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
                                    if !sanitized.is_empty() && sanitized != closure_name && !used_import_names.contains(&sanitized) {
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
            sorted_renames.sort_by(|(a, _), (b, _)| a.cmp(b));
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

        // Deduplicate exports (e.g. multiple export * from same module)
        {
            let mut seen = HashSet::new();
            exports.retain(|e| seen.insert(e.clone()));
        }

        // `function name(){…}` + `export const name = …` → `export function name`
        dedupe_function_export_collisions(&mut body_stmts, &mut exports);

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

    for body in body_stmts.iter_mut() {
        for name in &promote {
            *body = body.replace(
                &format!("async function {name}("),
                &format!("export async function {name}("),
            );
            *body = body.replace(
                &format!("function {name}("),
                &format!("export function {name}("),
            );
            *body = body.replace(
                &format!("async function* {name}("),
                &format!("export async function* {name}("),
            );
            *body = body.replace(
                &format!("function* {name}("),
                &format!("export function* {name}("),
            );
            *body = body.replace("export export ", "export ");
        }
    }
}