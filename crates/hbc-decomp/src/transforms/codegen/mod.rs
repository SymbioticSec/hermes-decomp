mod esm_gen;
mod esm_classify;
mod esm_patterns;
mod esm_descriptors;
mod esm_boilerplate;
mod expr_gen;
mod format;
mod stmt_gen;
mod control_flow;

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
    crate::analysis::metro::registry::FactoryRoles::standard().is_exports_param(name)
}

pub(super) fn is_module_like(name: &str) -> bool {
    crate::analysis::metro::registry::FactoryRoles::standard().is_module_param(name)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Constant, Expression};

    #[test]
    fn test_simple_codegen() {
        let stmts = vec![
            Statement::let_stmt("x", Expression::constant(Constant::Integer(42))),
            Statement::Return(Some(Expression::Value(crate::ir::Value::Register(0)))),
        ];

        let mut codegen = Codegen::new(CodegenOptions::new());
        let output = codegen.generate_statements(&stmts);

        assert!(output.contains("let x = 42;"));
        assert!(output.contains("return r0;"));
    }

    #[test]
    fn test_if_codegen() {
        let stmts = vec![Statement::If {
            condition: Expression::Value(crate::ir::Value::Register(0)),
            then_body: vec![Statement::Return(Some(Expression::constant(
                Constant::Integer(1),
            )))],
            else_body: vec![Statement::Return(Some(Expression::constant(
                Constant::Integer(0),
            )))],
        }];

        let mut codegen = Codegen::new(CodegenOptions::new());
        let output = codegen.generate_statements(&stmts);

        assert!(output.contains("if (r0)"));
        assert!(output.contains("return 1;"));
        assert!(output.contains("return 0;"));
    }
    #[test]
    fn test_require_import_comment() {
        // require(5) with import_map { 5 -> "react-native" } should add /* react-native */
        let mut imports = BTreeMap::new();
        imports.insert(5u32, "react-native".to_string());

        let codegen = Codegen::new(CodegenOptions::new()).with_imports(imports);
        let expr = Expression::call(
            Expression::Value(crate::ir::Value::Variable("require".into())),
            vec![
                Expression::constant(Constant::Undefined),
                Expression::constant(Constant::Integer(5)),
            ],
        );
        let result = codegen.generate_expr(&expr);
        assert!(result.contains("/* react-native */"), "Expected import comment, got: {}", result);
    }

    #[test]
    fn test_for_of_uses_generate_expr() {
        let stmts = vec![Statement::ForOf {
            variable: "item".into(),
            iterable: Expression::call(
                Expression::Value(crate::ir::Value::Variable("require".into())),
                vec![
                    Expression::constant(Constant::Undefined),
                    Expression::constant(Constant::Integer(3)),
                ],
            ),
            body: vec![Statement::Comment("body".into())],
        }];

        let mut imports = BTreeMap::new();
        imports.insert(3u32, "utils".to_string());
        let mut codegen = Codegen::new(CodegenOptions::new()).with_imports(imports);
        let output = codegen.generate_statements(&stmts);
        // ForOf should use generate_expr for iterable, which injects import comments
        assert!(output.contains("/* utils */"), "ForOf should use generate_expr for iterable, got: {}", output);
    }

    #[test]
    fn test_switch_uses_generate_expr() {
        let stmts = vec![Statement::Switch {
            discriminant: Expression::Value(crate::ir::Value::Variable("x".into())),
            cases: vec![(
                Expression::constant(Constant::Integer(1)),
                vec![Statement::Return(Some(Expression::constant(Constant::Integer(42))))],
            )],
            default: None,
        }];

        let mut codegen = Codegen::new(CodegenOptions::new());
        let output = codegen.generate_statements(&stmts);
        assert!(output.contains("switch (x)"), "got: {}", output);
        assert!(output.contains("case 1:"), "got: {}", output);
        assert!(output.contains("return 42;"), "got: {}", output);
    }

    #[test]
    fn test_class_super_uses_generate_expr() {
        let stmts = vec![Statement::Class {
            name: "MyClass".into(),
            super_class: Some(Expression::Value(crate::ir::Value::Variable("BaseClass".into()))),
            constructor: None,
            methods: vec![],
        }];

        let mut codegen = Codegen::new(CodegenOptions::new());
        let output = codegen.generate_statements(&stmts);
        assert!(output.contains("class MyClass extends BaseClass"), "got: {}", output);
    }

    #[test]
    fn test_assign_target_member() {
        let codegen = Codegen::new(CodegenOptions::new());
        let target = crate::ir::AssignTarget::Member {
            object: Expression::Value(crate::ir::Value::Variable("obj".into())),
            property: "prop".into(),
        };
        let result = codegen.generate_assign_target(&target);
        assert_eq!(result, "obj.prop");
    }

    #[test]
    fn arrow_after_logical_or_is_parenthesized() {
        // `x || (arg0) => {…}` is a SyntaxError; need `x || ((arg0) => …)`.
        use crate::ir::{FunctionId, Value};
        let codegen = Codegen::new(CodegenOptions::new());
        let arrow = Expression::Function {
            id: FunctionId(99),
            name: None,
            is_arrow: true,
            is_async: false,
            is_generator: false,
        };
        let expr = Expression::Binary {
            op: crate::ir::BinaryOp::LogicalOr,
            left: Box::new(Expression::Value(Value::Variable("x".into()))),
            right: Box::new(arrow),
        };
        let out = codegen.generate_expr(&expr);
        assert!(
            out.contains("|| ((") || out.contains("|| (() =>"),
            "arrow RHS of || must be parenthesized, got: {out}"
        );
        // Must not be the bare invalid form `x || () =>`
        assert!(
            !out.contains("|| () =>") && !out.contains("|| (arg"),
            "unparenthesized arrow after || is invalid JS: {out}"
        );
    }

    #[test]
    fn template_quasi_escapes_inner_backticks() {
        let codegen = Codegen::new(CodegenOptions::new());
        let expr = Expression::TemplateLiteral {
            quasis: vec!["warn: `nested` ".into(), "".into()],
            expressions: vec![Expression::Value(crate::ir::Value::Variable("x".into()))],
        };
        let out = codegen.generate_expr(&expr);
        assert!(
            out.contains("\\`nested\\`"),
            "inner backticks must be escaped, got: {out}"
        );
        assert!(out.starts_with('`') && out.ends_with('`'), "got: {out}");
        assert!(out.contains("${x}"), "got: {out}");
    }

    #[test]
    fn test_assign_target_destructuring_array() {
        let codegen = Codegen::new(CodegenOptions::new());
        let target = crate::ir::AssignTarget::DestructuringArray(vec![
            Some((crate::ir::AssignTarget::Variable("a".into()), None)),
            None,
            Some((crate::ir::AssignTarget::Variable("b".into()), None)),
        ]);
        let result = codegen.generate_assign_target(&target);
        assert_eq!(result, "[a, , b]");
    }

    #[test]
    fn test_esm_import_from_require() {
        // let x = require(0) should become `import x from "react"`
        let stmts = vec![Statement::let_stmt(
            "React",
            Expression::call(
                Expression::Value(crate::ir::Value::Variable("require".into())),
                vec![
                    Expression::constant(Constant::Undefined),
                    Expression::constant(Constant::Integer(0)),
                ],
            ),
        )];

        let mut dep_names = BTreeMap::new();
        dep_names.insert(0u32, "react".to_string());

        let mut codegen = Codegen::new(CodegenOptions::new()).with_esm_mode(dep_names);
        let output = codegen.generate_esm_module(&stmts, 42, Some("my-module"));
        assert!(output.contains("import React from \"react\""), "Expected import, got: {}", output);
        assert!(output.contains("// Module 42 (my-module)"), "Expected header, got: {}", output);
    }

    #[test]
    fn test_esm_export_from_assign() {
        // exports.default = value should become `export default value`
        let stmts = vec![Statement::Assign {
            target: crate::ir::AssignTarget::Member {
                object: Expression::Value(crate::ir::Value::Variable("exports".into())),
                property: "default".into(),
            },
            value: Expression::Value(crate::ir::Value::Variable("MyComponent".into())),
        }];

        let mut codegen = Codegen::new(CodegenOptions::new()).with_esm_mode(BTreeMap::new());
        let output = codegen.generate_esm_module(&stmts, 10, Some("my-component"));
        assert!(output.contains("export default MyComponent"), "Expected export, got: {}", output);
    }

    #[test]
    fn test_esm_skip_esmodule_boilerplate() {
        // Assignments containing __esModule should be skipped
        let stmts = vec![
            Statement::Assign {
                target: crate::ir::AssignTarget::Member {
                    object: Expression::Value(crate::ir::Value::Variable("exports".into())),
                    property: "__esModule".into(),
                },
                value: Expression::constant(Constant::Bool(true)),
            },
            Statement::Return(None),
        ];

        let mut codegen = Codegen::new(CodegenOptions::new()).with_esm_mode(BTreeMap::new());
        let output = codegen.generate_esm_module(&stmts, 1, None);
        // Should NOT contain __esModule or return
        assert!(!output.contains("__esModule"), "Expected skip, got: {}", output);
        assert!(!output.contains("return"), "Expected skip return, got: {}", output);
    }

    #[test]
    fn test_esm_named_export() {
        // exports.foo = bar -> export const foo = bar
        let stmts = vec![Statement::Assign {
            target: crate::ir::AssignTarget::Member {
                object: Expression::Value(crate::ir::Value::Variable("exports".into())),
                property: "loginWithToken".into(),
            },
            value: Expression::Value(crate::ir::Value::Variable("fn42".into())),
        }];

        let mut codegen = Codegen::new(CodegenOptions::new()).with_esm_mode(BTreeMap::new());
        let output = codegen.generate_esm_module(&stmts, 5, Some("auth"));
        assert!(output.contains("export const loginWithToken = fn42"), "Expected named export, got: {}", output);
    }
}
