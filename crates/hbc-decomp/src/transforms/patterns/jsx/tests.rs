use super::*;
use crate::ir::{AssignTarget, VarKind};

#[test]
fn test_classic_jsx_element() {
    let mut expr = Expression::call(
        Expression::member(
            Expression::Value(Value::Binding(Binding::Variable("React".to_string()))),
            "createElement",
        ),
        vec![
            Expression::constant(Constant::String("div".to_string())),
            Expression::Object {
                properties: vec![ObjectProperty {
                    key: PropertyKey::Ident("id".to_string()),
                    value: Expression::constant(Constant::String("main".to_string())),
                }],
            },
            Expression::constant(Constant::String("Text".to_string())),
        ],
    );
    JSXReconstructor::new().visit_expression(&mut expr);
    assert!(matches!(expr, Expression::JSXElement { .. }));
}

#[test]
fn folds_assigned_props_and_keeps_the_earlier_child() {
    let tag = Expression::Value(Value::Binding(Binding::Variable("View".into())));
    let child = Expression::Value(Value::Binding(Binding::Variable("tmp3".into())));
    let stmts = vec![
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable("obj".into())),
            value: Expression::Object { properties: vec![] },
        },
        Statement::Assign {
            target: AssignTarget::Member {
                object: Expression::Value(Value::Binding(Binding::Variable("obj".into()))),
                property: "children".into(),
            },
            value: child,
        },
        Statement::Assign {
            target: AssignTarget::Member {
                object: Expression::Value(Value::Binding(Binding::Variable("obj".into()))),
                property: "children".into(),
            },
            value: Expression::call(
                Expression::Value(Value::Binding(Binding::Variable("jsx".into()))),
                vec![tag, Expression::Object { properties: vec![] }],
            ),
        },
        Statement::Return(Some(Expression::call(
            Expression::Value(Value::Binding(Binding::Variable("jsx".into()))),
            vec![
                Expression::Value(Value::Binding(Binding::Variable("Provider".into()))),
                Expression::Value(Value::Binding(Binding::Variable("obj".into()))),
            ],
        ))),
    ];
    let out = reconstruct_jsx(stmts);
    // The prop assigns fold into the literal, the overwrite is dropped, and
    // the object definition goes with the call that absorbed it.
    assert_eq!(out.len(), 1, "prop assigns folded, definition consumed");
    match &out[0] {
        Statement::Return(Some(Expression::JSXElement { children, .. })) => {
            assert!(!children.is_empty(), "the earlier child is kept");
        }
        other => panic!("expected jsx return, got {other:?}"),
    }
}

#[test]
fn resolves_props_variable_one_hop() {
    let stmts = vec![
        Statement::Let {
            name: "p".into(),
            value: Expression::Object {
                properties: vec![ObjectProperty {
                    key: PropertyKey::Ident("id".into()),
                    value: Expression::constant(Constant::String("x".into())),
                }],
            },
            kind: VarKind::Let,
        },
        Statement::Expr(Expression::call(
            Expression::Value(Value::Binding(Binding::Variable("_jsx".into()))),
            vec![
                Expression::constant(Constant::String("div".into())),
                Expression::Value(Value::Binding(Binding::Variable("p".into()))),
            ],
        )),
    ];
    let out = reconstruct_jsx(stmts);
    assert_eq!(out.len(), 1, "the props definition is consumed by the call");
    match &out[0] {
        Statement::Expr(Expression::JSXElement {
            tag, attributes, ..
        }) => {
            assert_eq!(tag, "div");
            assert!(attributes.iter().any(|(k, _)| k == "id"));
        }
        other => panic!("expected jsx expr, got {other:?}"),
    }
}

#[test]
fn test_modern_key_third_arg() {
    let mut expr = Expression::call(
        Expression::Value(Value::Binding(Binding::Variable("_jsx".into()))),
        vec![
            Expression::Value(Value::Binding(Binding::Variable("Foo".into()))),
            Expression::Object {
                properties: vec![ObjectProperty {
                    key: PropertyKey::Ident("title".into()),
                    value: Expression::constant(Constant::String("x".into())),
                }],
            },
            Expression::Value(Value::Binding(Binding::Variable("k".into()))),
        ],
    );
    JSXReconstructor::new().visit_expression(&mut expr);
    match expr {
        Expression::JSXElement { attributes, .. } => {
            assert!(attributes.iter().any(|(k, _)| k == "key"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn test_fragment_empty_tag() {
    let mut expr = Expression::call(
        Expression::Value(Value::Binding(Binding::Variable("jsxs".into()))),
        vec![
            Expression::Value(Value::Binding(Binding::Variable("_Fragment".into()))),
            Expression::Object {
                properties: vec![ObjectProperty {
                    key: PropertyKey::Ident("children".into()),
                    value: Expression::Array {
                        elements: vec![Some(Expression::Value(Value::Binding(Binding::Variable(
                            "a".into(),
                        ))))],
                    },
                }],
            },
        ],
    );
    JSXReconstructor::new().visit_expression(&mut expr);
    match expr {
        Expression::JSXElement { tag, children, .. } => {
            assert_eq!(tag, "");
            assert_eq!(children.len(), 1);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn test_modern_jsx_member_factory() {
    let mut expr = Expression::call(
        Expression::member(
            Expression::Value(Value::Binding(Binding::Variable("jsxProd".into()))),
            "jsxs",
        ),
        vec![
            Expression::constant(Constant::String("ul".into())),
            Expression::Object {
                properties: vec![
                    ObjectProperty {
                        key: PropertyKey::Ident("className".into()),
                        value: Expression::constant(Constant::String("x".into())),
                    },
                    ObjectProperty {
                        key: PropertyKey::Ident("children".into()),
                        value: Expression::Array {
                            elements: vec![
                                Some(Expression::Value(Value::Binding(Binding::Variable(
                                    "a".into(),
                                )))),
                                Some(Expression::Value(Value::Binding(Binding::Variable(
                                    "b".into(),
                                )))),
                            ],
                        },
                    },
                ],
            },
        ],
    );
    JSXReconstructor::new().visit_expression(&mut expr);
    match expr {
        Expression::JSXElement {
            tag,
            attributes,
            children,
        } => {
            assert_eq!(tag, "ul");
            assert_eq!(attributes.len(), 1);
            assert_eq!(children.len(), 2);
        }
        other => panic!("{other:?}"),
    }
}
