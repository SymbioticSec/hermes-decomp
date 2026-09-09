use super::*;
use crate::ir::{Constant, VarKind};

fn null() -> Expression {
    Expression::Value(Value::Constant(Constant::Null))
}

fn themes(prop: &str) -> Expression {
    Expression::Member {
        object: Box::new(Expression::Value(Value::Variable("Themes".into()))),
        property: PropertyKey::Ident(prop.into()),
        optional: false,
    }
}

fn index_assign(name: &str, slot: i32, value: Expression) -> Statement {
    Statement::Assign {
        target: AssignTarget::Index {
            object: Expression::Value(Value::Variable(name.into())),
            key: Expression::Value(Value::Constant(Constant::Integer(slot))),
        },
        value,
    }
}

#[test]
fn folds_named_let_slot_fills() {
    let mut stmts = vec![
        Statement::Let {
            name: "obj".into(),
            value: Expression::Object {
                properties: vec![
                    ObjectProperty {
                        key: PropertyKey::Ident("default".into()),
                        value: null(),
                    },
                    ObjectProperty {
                        key: PropertyKey::Ident("active".into()),
                        value: null(),
                    },
                ],
            },
            kind: VarKind::Let,
        },
        index_assign("obj", 0, themes("DEFAULT")),
        index_assign("obj", 1, themes("ACTIVE")),
    ];
    fold_slot_index_fills(&mut stmts);
    assert_eq!(stmts.len(), 1, "fills should be consumed: {stmts:?}");
    match &stmts[0] {
        Statement::Let {
            value: Expression::Object { properties },
            ..
        } => {
            assert!(matches!(&properties[0].value, Expression::Member { .. }));
            assert!(matches!(&properties[1].value, Expression::Member { .. }));
        }
        other => panic!("expected folded Let object, got {other:?}"),
    }
}

#[test]
fn folds_named_member_placeholder_assigns() {
    let mut stmts = vec![
        Statement::Let {
            name: "obj1".into(),
            value: Expression::Object {
                properties: vec![
                    ObjectProperty {
                        key: PropertyKey::Ident("url".into()),
                        value: Expression::Value(Value::Variable("URL".into())),
                    },
                    ObjectProperty {
                        key: PropertyKey::Ident("body".into()),
                        value: null(),
                    },
                ],
            },
            kind: VarKind::Let,
        },
        Statement::Assign {
            target: AssignTarget::Member {
                object: Expression::Value(Value::Variable("obj1".into())),
                property: "body".into(),
            },
            value: Expression::Object {
                properties: vec![ObjectProperty {
                    key: PropertyKey::Ident("login".into()),
                    value: Expression::Value(Value::Variable("user".into())),
                }],
            },
        },
    ];
    fold_slot_index_fills(&mut stmts);
    assert_eq!(stmts.len(), 1, "member fill should be consumed: {stmts:?}");
    match &stmts[0] {
        Statement::Let {
            value: Expression::Object { properties },
            ..
        } => {
            assert!(
                matches!(&properties[1].value, Expression::Object { .. }),
                "body should be the login object: {:?}",
                properties[1].value
            );
        }
        other => panic!("expected folded object, got {other:?}"),
    }
}

#[test]
fn does_not_fold_forward_referenced_value() {
    // `obj = {a: null, b: null}; config = {...}; obj[0] = config; obj[1] = Themes.ACTIVE`.
    // Slot 0's value `config` is defined AFTER the object, so folding it into the
    // literal would read it before it exists (and drops it downstream). It must stay
    // a separate fill, while slot 1 (a clean member value) still folds.
    let mut stmts = vec![
        Statement::Let {
            name: "obj".into(),
            value: Expression::Object {
                properties: vec![
                    ObjectProperty { key: PropertyKey::Ident("a".into()), value: null() },
                    ObjectProperty { key: PropertyKey::Ident("b".into()), value: null() },
                ],
            },
            kind: VarKind::Let,
        },
        Statement::Let {
            name: "config".into(),
            value: Expression::Object {
                properties: vec![ObjectProperty {
                    key: PropertyKey::Ident("k".into()),
                    value: Expression::Value(Value::Constant(Constant::Integer(1))),
                }],
            },
            kind: VarKind::Let,
        },
        index_assign("obj", 0, Expression::Value(Value::Variable("config".into()))),
        index_assign("obj", 1, themes("ACTIVE")),
    ];
    fold_slot_index_fills(&mut stmts);
    // The forward-ref fill for slot 0 stays (as `obj.a = config`, not hoisted);
    // the config def stays; slot 1 folds into the literal.
    let has_forward_fill = stmts.iter().any(|s| matches!(
        s,
        Statement::Assign {
            target: AssignTarget::Member { property, .. },
            value: Expression::Value(Value::Variable(v)),
        } if property == "a" && v == "config"
    ));
    assert!(has_forward_fill, "forward-ref fill must be preserved: {stmts:?}");
    let obj = stmts.iter().find_map(|s| match s {
        Statement::Let { name, value: Expression::Object { properties }, .. } if name == "obj" => Some(properties),
        _ => None,
    }).expect("obj literal present");
    assert!(matches!(&obj[0].value, Expression::Value(Value::Constant(Constant::Null))), "slot 0 stays placeholder");
    assert!(matches!(&obj[1].value, Expression::Member { .. }), "slot 1 clean value folds");
}

#[test]
fn does_not_fold_slot_fills_inside_switch() {
    // Nested folding is intentionally off: it rewrote generator `{value,done}`
    // objects and left raw v98 state machines in the dump.
    let inner = vec![
        Statement::Let {
            name: "obj".into(),
            value: Expression::Object {
                properties: vec![
                    ObjectProperty {
                        key: PropertyKey::Ident("notif_type".into()),
                        value: null(),
                    },
                    ObjectProperty {
                        key: PropertyKey::Ident("guild_id".into()),
                        value: null(),
                    },
                ],
            },
            kind: VarKind::Let,
        },
        index_assign("obj", 0, Expression::Value(Value::Variable("type".into()))),
        index_assign("obj", 1, Expression::Value(Value::Variable("guild_id".into()))),
    ];
    let mut stmts = vec![Statement::Switch {
        discriminant: Expression::Value(Value::Variable("type".into())),
        cases: vec![(
            Expression::Value(Value::Constant(Constant::String("MESSAGE_CREATE".into()))),
            inner.clone(),
        )],
        default: None,
    }];
    fold_slot_index_fills(&mut stmts);
    match &stmts[0] {
        Statement::Switch { cases, .. } => {
            assert_eq!(cases[0].1.len(), 3, "nested fills must stay: {:?}", cases[0].1);
        }
        other => panic!("expected switch, got {other:?}"),
    }
}

#[test]
fn rewrites_forward_ref_slot_index_to_named_member() {
    // `user_id` is defined after the literal, so the fill cannot be hoisted,
    // but `obj[1]` is still slot 1 of a named-key shape and must render as
    // `obj.guild_id`.
    let mut stmts = vec![
        Statement::Let {
            name: "obj".into(),
            value: Expression::Object {
                properties: vec![
                    ObjectProperty {
                        key: PropertyKey::Ident("notif_type".into()),
                        value: null(),
                    },
                    ObjectProperty {
                        key: PropertyKey::Ident("guild_id".into()),
                        value: null(),
                    },
                ],
            },
            kind: VarKind::Let,
        },
        Statement::Let {
            name: "user_id".into(),
            value: Expression::Value(Value::Variable("type".into())),
            kind: VarKind::Let,
        },
        index_assign("obj", 1, Expression::Value(Value::Variable("user_id".into()))),
    ];
    fold_slot_index_fills(&mut stmts);
    assert_eq!(stmts.len(), 3, "forward-ref fill stays: {stmts:?}");
    match &stmts[2] {
        Statement::Assign {
            target: AssignTarget::Member { property, .. },
            ..
        } if property == "guild_id" => {}
        other => panic!("expected obj.guild_id = user_id, got {other:?}"),
    }
}

#[test]
fn does_not_fold_non_placeholder_numeric_key() {
    let mut stmts = vec![
        Statement::Let {
            name: "obj".into(),
            value: Expression::Object {
                properties: vec![ObjectProperty {
                    key: PropertyKey::Ident("a".into()),
                    value: Expression::Value(Value::Constant(Constant::Integer(1))),
                }],
            },
            kind: VarKind::Let,
        },
        index_assign("obj", 0, themes("DEFAULT")),
    ];
    fold_slot_index_fills(&mut stmts);
    assert_eq!(stmts.len(), 2, "non-placeholder must stay: {stmts:?}");
}
