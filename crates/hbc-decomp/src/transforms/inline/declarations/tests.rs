use super::*;
use crate::ir::{Constant, Expression, Value};

#[test]
fn nested_mutation_forces_let_not_const() {
    // Parent: tmp = 0;  Child free-mutates tmp → parent must use let.
    let mut parent = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("tmp".into())),
        value: Expression::constant(Constant::Integer(0)),
    }];
    let child = vec![
        // first use is read (free), then write
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable("tmp".into())),
            value: Expression::Binary {
                op: crate::ir::BinaryOp::Add,
                left: Box::new(Expression::Value(Value::Binding(Binding::Variable(
                    "tmp".into(),
                )))),
                right: Box::new(Expression::constant(Constant::Integer(1))),
            },
        },
    ];
    let extra = extra_writes_from_nested_bodies(&[&child]);
    assert!(extra.get("tmp").copied().unwrap_or(0) >= 2);
    insert_declarations_with_extra_writes(&mut parent, &[], &extra);
    match &parent[0] {
        Statement::Let { kind, name, .. } => {
            assert_eq!(name, "tmp");
            assert_eq!(*kind, VarKind::Let, "expected let when nested writes");
        }
        other => panic!("expected Let, got {other:?}"),
    }
}

#[test]
fn no_nested_writes_keeps_const() {
    let mut parent = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("x".into())),
        value: Expression::constant(Constant::Integer(1)),
    }];
    insert_declarations_with_extra_writes(&mut parent, &[], &BTreeMap::new());
    match &parent[0] {
        Statement::Let { kind, .. } => assert_eq!(*kind, VarKind::Const),
        other => panic!("expected Let, got {other:?}"),
    }
}

#[test]
fn write_first_nested_reassign_forces_let() {
    // Parent: items = [];  Child (constructor/getter): items = [1] as first use.
    // Free-only counting used to miss this; parent must still be let.
    let mut parent = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("items".into())),
        value: Expression::Array { elements: vec![] },
    }];
    let child = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("items".into())),
        value: Expression::Array {
            elements: vec![Some(Expression::constant(Constant::Integer(1)))],
        },
    }];
    let extra = extra_writes_from_nested_bodies(&[&child]);
    assert!(
        extra.get("items").copied().unwrap_or(0) >= 1,
        "nested write-first must contribute extra writes, got {extra:?}"
    );
    insert_declarations_with_extra_writes(&mut parent, &[], &extra);
    match &parent[0] {
        Statement::Let { kind, name, .. } => {
            assert_eq!(name, "items");
            assert_eq!(
                *kind,
                VarKind::Let,
                "expected let for cross-scope reassignment"
            );
        }
        other => panic!("expected Let, got {other:?}"),
    }
}

#[test]
fn nested_local_let_does_not_force_parent_let() {
    // Child owns `items` via Let, pure shadowing, parent stays const.
    let mut parent = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("items".into())),
        value: Expression::Array { elements: vec![] },
    }];
    let child = vec![Statement::Let {
        name: "items".into(),
        value: Expression::Array { elements: vec![] },
        kind: VarKind::Let,
    }];
    let extra = extra_writes_from_nested_bodies(&[&child]);
    assert!(
        !extra.contains_key("items"),
        "explicit nested Let must not leak writes, got {extra:?}"
    );
    insert_declarations_with_extra_writes(&mut parent, &[], &extra);
    match &parent[0] {
        Statement::Let { kind, .. } => assert_eq!(*kind, VarKind::Const),
        other => panic!("expected Let, got {other:?}"),
    }
}

#[test]
fn nested_env_slot_write_stays_assign() {
    let mut child = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("closure_1".into())),
        value: Expression::Value(Value::Binding(Binding::Variable("payload".into()))),
    }];
    let mut outer = HashSet::new();
    outer.insert("closure_1".into());
    insert_declarations_with_outer(&mut child, &[], &BTreeMap::new(), &outer, true);
    match &child[0] {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(n)),
            ..
        } => assert_eq!(n, "closure_1"),
        other => panic!("expected Assign, got {other:?}"),
    }
}

#[test]
fn switch_cases_declare_the_same_name_separately() {
    let mut body = vec![Statement::Switch {
        discriminant: Expression::Value(Value::Binding(Binding::Variable("kind".into()))),
        cases: vec![
            (
                Expression::constant(crate::ir::Constant::String("voice".into())),
                vec![Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable("userId".into())),
                    value: Expression::constant(crate::ir::Constant::Integer(1)),
                }],
            ),
            (
                Expression::constant(crate::ir::Constant::String("unified".into())),
                vec![Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable("userId".into())),
                    value: Expression::constant(crate::ir::Constant::Integer(2)),
                }],
            ),
        ],
        default: None,
    }];
    insert_declarations(&mut body, &[]);
    let Statement::Switch { cases, .. } = &body[0] else {
        panic!()
    };
    assert!(matches!(&cases[0].1[0], Statement::Let { name, .. } if name == "userId"));
    assert!(matches!(&cases[1].1[0], Statement::Let { name, .. } if name == "userId"));
}

#[test]
fn own_env_slot_is_declared_when_it_is_not_an_ancestor_capture() {
    // A `closure_N` written here and not listed as an ancestor slot is this
    // function's own binding. Leaving it as a bare assignment makes it an
    // implicit global. Ancestor captures stay assignments; see the test above.
    let mut child = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("closure_1_2".into())),
        value: Expression::Array { elements: vec![] },
    }];
    insert_declarations_with_outer(&mut child, &[], &BTreeMap::new(), &HashSet::new(), true);
    match &child[0] {
        Statement::Let { name, kind, .. } => {
            assert_eq!(name, "closure_1_2");
            assert_eq!(*kind, crate::ir::VarKind::Let);
        }
        other => panic!("own env slot must be declared, got {other:?}"),
    }
}

#[test]
fn factory_env_slot_still_declared() {
    let mut parent = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("closure_1".into())),
        value: Expression::Array { elements: vec![] },
    }];
    insert_declarations_with_extra_writes(&mut parent, &[], &BTreeMap::new());
    match &parent[0] {
        Statement::Let { name, .. } => assert_eq!(name, "closure_1"),
        other => panic!("factory must still declare env slot, got {other:?}"),
    }
}

#[test]
fn generic_local_declared_when_outer_is_only_env_slots() {
    // Ancestor locals like `obj` must not suppress the child's own `let obj`.
    let mut child = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("obj".into())),
        value: Expression::Object { properties: vec![] },
    }];
    let mut outer = HashSet::new();
    outer.insert("closure_1".into());
    insert_declarations_with_outer(&mut child, &[], &BTreeMap::new(), &outer, true);
    match &child[0] {
        Statement::Let { name, .. } => assert_eq!(name, "obj"),
        other => panic!("expected Let obj, got {other:?}"),
    }
}
