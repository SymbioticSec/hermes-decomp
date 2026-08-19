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
