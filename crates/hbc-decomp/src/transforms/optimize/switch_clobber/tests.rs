use super::*;

fn var(name: &str) -> Expression {
    Expression::Value(Value::Binding(Binding::Variable(name.into())))
}

#[test]
fn discriminant_store_does_not_clobber_later_member_reads() {
    let case = vec![
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable("kind".into())),
            value: Expression::Member {
                object: Box::new(var("kind")),
                property: PropertyKey::Ident("kind".into()),
                optional: false,
            },
        },
        Statement::Let {
            name: "userId".into(),
            value: Expression::Member {
                object: Box::new(var("kind")),
                property: PropertyKey::Ident("userId".into()),
                optional: false,
            },
            kind: crate::ir::VarKind::Const,
        },
        Statement::Return(Some(Expression::binary(
            crate::ir::BinaryOp::Add,
            var("kind"),
            var("userId"),
        ))),
    ];
    let stmts = repair_switch_clobbers(vec![Statement::Switch {
        discriminant: Expression::Member {
            object: Box::new(var("kind")),
            property: PropertyKey::Ident("kind".into()),
            optional: false,
        },
        cases: vec![(var("embedded"), case)],
        default: None,
    }]);
    let Statement::Switch { cases, .. } = &stmts[0] else {
        panic!()
    };
    match &cases[0].1[0] {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(n)),
            ..
        } => {
            assert_eq!(n, "kindValue");
        }
        other => panic!("expected renamed store, got {other:?}"),
    }
    match &cases[0].1[1] {
        Statement::Let { value, .. } => {
            assert!(matches!(
                value,
                Expression::Member { object, .. }
                    if matches!(object.as_ref(), Expression::Value(Value::Binding(Binding::Variable(n))) if n == "kind")
            ));
        }
        other => panic!("member base must stay the object, got {other:?}"),
    }
}

#[test]
fn let_shadow_of_same_property_keeps_later_member_reads() {
    let case = vec![
        Statement::Let {
            name: "kind".into(),
            value: Expression::Member {
                object: Box::new(var("kind")),
                property: PropertyKey::Ident("kind".into()),
                optional: false,
            },
            kind: crate::ir::VarKind::Const,
        },
        Statement::Let {
            name: "userId".into(),
            value: Expression::binary(
                crate::ir::BinaryOp::NullishCoalesce,
                Expression::Member {
                    object: Box::new(Expression::Member {
                        object: Box::new(var("kind")),
                        property: PropertyKey::Ident("voiceState".into()),
                        optional: false,
                    }),
                    property: PropertyKey::Ident("channelId".into()),
                    optional: false,
                },
                Expression::Member {
                    object: Box::new(var("kind")),
                    property: PropertyKey::Ident("userId".into()),
                    optional: false,
                },
            ),
            kind: crate::ir::VarKind::Const,
        },
        Statement::Return(Some(Expression::binary(
            crate::ir::BinaryOp::Add,
            var("kind"),
            Expression::Member {
                object: Box::new(var("kind")),
                property: PropertyKey::Ident("activity".into()),
                optional: false,
            },
        ))),
    ];
    let stmts = repair_switch_clobbers(vec![Statement::Switch {
        discriminant: var("kind"),
        cases: vec![(var("embedded"), case)],
        default: None,
    }]);
    let Statement::Switch { cases, .. } = &stmts[0] else {
        panic!()
    };
    match &cases[0].1[0] {
        Statement::Let { name, .. } => assert_eq!(name, "kindValue"),
        other => panic!("expected renamed let, got {other:?}"),
    }
    match &cases[0].1[2] {
        Statement::Return(Some(Expression::Binary { left, right, .. })) => {
            assert!(
                matches!(left.as_ref(), Expression::Value(Value::Binding(Binding::Variable(n))) if n == "kindValue")
            );
            assert!(matches!(
                right.as_ref(),
                Expression::Member { object, .. }
                    if matches!(object.as_ref(), Expression::Value(Value::Binding(Binding::Variable(n))) if n == "kind")
            ));
        }
        other => panic!("return must use the string and the object, got {other:?}"),
    }
}

#[test]
fn param_rename_makes_the_property_copy_collide() {
    let mut stmts = vec![Statement::Switch {
        discriminant: Expression::Member {
            object: Box::new(var("arg0")),
            property: PropertyKey::Ident("kind".into()),
            optional: false,
        },
        cases: vec![(
            var("embedded"),
            vec![
                Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable("kind".into())),
                    value: Expression::Member {
                        object: Box::new(var("arg0")),
                        property: PropertyKey::Ident("kind".into()),
                        optional: false,
                    },
                },
                Statement::Return(Some(Expression::binary(
                    crate::ir::BinaryOp::Add,
                    var("kind"),
                    Expression::Member {
                        object: Box::new(var("arg0")),
                        property: PropertyKey::Ident("activity".into()),
                        optional: false,
                    },
                ))),
            ],
        )],
        default: None,
    }];
    crate::transforms::exports::rename_param_registers(&mut stmts, &[Some("kind".into())]);
    let stmts = repair_switch_clobbers(stmts);
    let Statement::Switch { cases, .. } = &stmts[0] else {
        panic!()
    };
    match &cases[0].1[0] {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(n)),
            value,
        } => {
            assert_eq!(n, "kindValue");
            assert!(matches!(
                value,
                Expression::Member { object, property, .. }
                    if matches!(object.as_ref(), Expression::Value(Value::Binding(Binding::Variable(b))) if b == "kind")
                        && matches!(property, PropertyKey::Ident(p) if p == "kind")
            ));
        }
        other => panic!("expected split copy, got {other:?}"),
    }
    match &cases[0].1[1] {
        Statement::Return(Some(Expression::Binary { left, right, .. })) => {
            assert!(
                matches!(left.as_ref(), Expression::Value(Value::Binding(Binding::Variable(n))) if n == "kindValue")
            );
            assert!(matches!(
                right.as_ref(),
                Expression::Member { object, .. }
                    if matches!(object.as_ref(), Expression::Value(Value::Binding(Binding::Variable(n))) if n == "kind")
            ));
        }
        other => panic!("activity must stay on the object, got {other:?}"),
    }
}

#[test]
fn different_property_update_is_left_alone() {
    let case = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("node".into())),
        value: Expression::Member {
            object: Box::new(var("node")),
            property: PropertyKey::Ident("next".into()),
            optional: false,
        },
    }];
    let stmts = repair_switch_clobbers(vec![Statement::Switch {
        discriminant: var("node"),
        cases: vec![(var("x"), case)],
        default: None,
    }]);
    let Statement::Switch { cases, .. } = &stmts[0] else {
        panic!()
    };
    match &cases[0].1[0] {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(n)),
            ..
        } => {
            assert_eq!(n, "node");
        }
        other => panic!("expected untouched store, got {other:?}"),
    }
}
