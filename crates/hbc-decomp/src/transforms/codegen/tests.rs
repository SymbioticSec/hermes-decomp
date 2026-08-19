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
    // let x = require(0) with absolute module id 0 → import from import_map name
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

    let mut import_map = BTreeMap::new();
    import_map.insert(0u32, "react".to_string());
    // dep_names alone must not rename absolute ids (indices ≠ module ids).
    let mut dep_names = BTreeMap::new();
    dep_names.insert(0u32, "wrong-if-used".to_string());

    let mut codegen = Codegen::new(CodegenOptions::new())
        .with_imports(import_map)
        .with_esm_mode(dep_names);
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
