mod esm_gen;
mod esm_imports;
mod esm_classify;
mod esm_patterns;
mod esm_descriptors;
mod esm_boilerplate;
mod expr_gen;
mod format;
mod stmt_gen;
mod control_flow;

#[cfg(test)]
mod tests;

use crate::ir::Statement;
use std::collections::BTreeMap;
use std::sync::Arc;

pub(super) fn is_effectively_empty(stmts: &[Statement]) -> bool {
    stmts.iter().all(|s| match s {
        Statement::Block(inner) => inner.is_empty() || is_effectively_empty(inner),
        Statement::Continue(_) => true,
        _ => false,
    })
}

pub(super) fn is_exports_like(name: &str) -> bool {
    crate::analysis::metro::registry::FactoryRoles::matches_exports_name(name)
}

pub(super) fn is_module_like(name: &str) -> bool {
    crate::analysis::metro::registry::FactoryRoles::matches_module_name(name)
}

pub(super) fn indent_multiline(s: &str, prefix: &str) -> String {
    let mut lines = s.lines();
    let mut result = String::new();
    if let Some(first) = lines.next() {
        result.push_str(prefix);
        result.push_str(first);
    }
    for line in lines {
        result.push('\n');
        if !line.is_empty() {
            result.push_str(prefix);
        }
        result.push_str(line);
    }
    result
}

// Sanitize a module name into a valid JavaScript identifier for import renaming.
// e.g. "react-native" -> "reactNative", "@babel/runtime/helpers/interop" -> "interop"
pub(super) fn sanitize_import_name(mod_name: &str) -> String {
    // Take the last path component
    let base = mod_name.rsplit('/').next().unwrap_or(mod_name);
    if base.is_empty() {
        return String::new();
    }

    // Convert to valid camelCase identifier
    let mut result = String::new();
    let mut capitalize_next = false;
    for ch in base.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' {
            if capitalize_next && !result.is_empty() {
                result.extend(ch.to_uppercase());
                capitalize_next = false;
            } else {
                result.push(ch);
                capitalize_next = false;
            }
        } else if ch == '-' || ch == '.' || ch == ' ' {
            // Spaces appear in Metro-inferred names like "get ActivityIndicator"
            capitalize_next = true;
        }
        // Skip other non-identifier chars
    }

    // Ensure starts with valid identifier char
    if result.starts_with(|c: char| c.is_ascii_digit()) {
        result.insert(0, '_');
    }

    // Reject empty, too-short (single char), or JS reserved words
    if result.len() <= 1
        || matches!(
            result.as_str(),
            "default" | "export" | "import" | "class" | "function" | "return"
            | "var" | "let" | "const" | "if" | "else" | "for" | "while" | "do"
            | "switch" | "case" | "break" | "continue" | "new" | "delete"
            | "typeof" | "void" | "in" | "of" | "instanceof" | "this" | "super"
            | "with" | "throw" | "try" | "catch" | "finally" | "yield" | "await"
            | "async" | "from" | "true" | "false" | "null" | "undefined"
        )
    {
        return String::new();
    }

    result
}

// Sanitize a loop variable name: if it's an r10xxx SSA register, use the fallback name.
pub(super) fn sanitize_loop_var(var: &str, fallback: &str) -> String {
    if var.starts_with('r') && var[1..].chars().all(|c| c.is_ascii_digit()) {
        fallback.to_string()
    } else {
        var.to_string()
    }
}

// Replace all whole-word occurrences of `old` with `new_val` in `text`.
// A word boundary is a non-identifier character (not [a-zA-Z0-9_$]) or start/end of string.
pub(super) fn replace_whole_word(text: &str, old: &str, new_val: &str) -> String {
    if old.is_empty() {
        return text.to_string();
    }
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(pos) = remaining.find(old) {
        // Check left boundary
        let left_ok = if pos == 0 {
            true
        } else {
            let prev = remaining.as_bytes()[pos - 1];
            !prev.is_ascii_alphanumeric() && prev != b'_' && prev != b'$'
        };
        // Check right boundary
        let after = pos + old.len();
        let right_ok = if after >= remaining.len() {
            true
        } else {
            let next = remaining.as_bytes()[after];
            !next.is_ascii_alphanumeric() && next != b'_' && next != b'$'
        };
        if left_ok && right_ok {
            result.push_str(&remaining[..pos]);
            result.push_str(new_val);
            remaining = &remaining[after..];
        } else {
            result.push_str(&remaining[..pos + old.len()]);
            remaining = &remaining[after..];
        }
    }
    result.push_str(remaining);
    result
}

// Options for code generation.
#[derive(Debug, Clone)]
pub struct CodegenOptions {
    // Indentation string.
    pub indent: String,
    // Include block labels as comments.
    pub include_labels: bool,
}

impl Default for CodegenOptions {
    fn default() -> Self {
        Self {
            indent: "  ".to_string(),
            include_labels: false,
        }
    }
}

impl CodegenOptions {
    pub fn new() -> Self {
        Self::default()
    }
}

// Info about a descriptor object used in Object.defineProperty calls.
pub(super) struct DescriptorInfo {
    // The rendered return value from a getter function, if present.
    pub getter_return: Option<String>,
    // The rendered direct "value" property, if present.
    pub value_prop: Option<String>,
}

// Classification of an IR statement for ESM output generation.
pub(super) enum EsmClassification {
    // Statement resolves to an ESM import (e.g. `import x from "mod"`)
    Import(String),
    // Statement resolves to an ESM export (e.g. `export const x = ...`)
    Export(String),
    // Statement generates both an import and an export (e.g. `export default require(dep)(args)`)
    ImportAndExport(String, String),
    // Boilerplate that should be removed from output
    Skip,
    // Regular code to keep in the module body
    Body,
}

// Code generator.
pub struct Codegen {
    pub(super) options: CodegenOptions,
    pub(super) indent_level: usize,
    pub(super) import_map: Option<BTreeMap<u32, String>>,
    // When true, generate ESM-style output for module factories.
    pub(super) esm_mode: bool,
    // Module dependency index -> name map (used in ESM mode to resolve require IDs).
    pub(super) dep_names: Option<BTreeMap<u32, String>>,
    // Module dependency index -> absolute module id (used to annotate imports with
    // the stable module id `/* N */` when the resolved require is `dependencyMap[idx]`).
    pub(super) dep_ids: Option<BTreeMap<u32, u32>>,
    // Pre-rendered inline function bodies (function_id -> complete function expression string).
    pub(super) inline_bodies: Arc<BTreeMap<u32, String>>,
}

impl Codegen {
    pub fn new(options: CodegenOptions) -> Self {
        Codegen {
            options,
            indent_level: 0,
            import_map: None,
            esm_mode: false,
            dep_names: None,
            dep_ids: None,
            inline_bodies: Arc::new(BTreeMap::new()),
        }
    }

    pub fn with_imports(mut self, imports: BTreeMap<u32, String>) -> Self {
        self.import_map = Some(imports);
        self
    }

    pub fn with_esm_mode(mut self, dep_names: BTreeMap<u32, String>) -> Self {
        self.esm_mode = true;
        self.dep_names = Some(dep_names);
        self
    }

    // Provide the dependency-index -> absolute-module-id map so imports can be
    // annotated with the stable module id `/* N */`.
    pub fn with_esm_module_meta(mut self, dep_ids: BTreeMap<u32, u32>) -> Self {
        self.dep_ids = Some(dep_ids);
        self
    }

    pub fn with_inline_bodies(mut self, bodies: Arc<BTreeMap<u32, String>>) -> Self {
        self.inline_bodies = bodies;
        self
    }

    // Generate code for a list of statements.
    pub fn generate_statements(&mut self, statements: &[Statement]) -> String {
        let mut output = String::new();
        for stmt in statements {
            output.push_str(&self.generate_stmt(stmt));
        }
        output
    }

    pub(super) fn current_indent(&self) -> String {
        self.options.indent.repeat(self.indent_level)
    }
}
