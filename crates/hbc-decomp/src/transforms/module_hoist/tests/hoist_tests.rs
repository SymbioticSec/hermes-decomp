use std::collections::BTreeMap;

use crate::ir::{
    AssignTarget, Expression, FunctionId, ObjectProperty, PropertyKey, Statement, Value, VarKind,
};

use super::super::hoist::hoist_module_loaders;
use super::helpers::{call, empty_registry_with_factory, int, member, var};

fn hoist(
    all_ir: &mut BTreeMap<u32, Vec<Statement>>,
    reg: &crate::analysis::MetroRegistry,
    children: &BTreeMap<u32, Vec<u32>>,
) {
    hoist_module_loaders(all_ir, reg, children, &BTreeMap::new());
}

fn func(id: u32) -> Expression {
    Expression::Function {
        id: FunctionId(id),
        name: None,
        is_arrow: true,
        is_async: false,
        is_generator: false,
    }
}

fn get_descriptor(getter_id: u32) -> Expression {
    Expression::Object {
        properties: vec![ObjectProperty {
            key: PropertyKey::Ident("get".into()),
            value: func(getter_id),
        }],
    }
}

#[test]
fn hoist_rewrites_nested_import_default_and_injects() {
    let factory_id = 1u32;
    let child_id = 2u32;
    let mut all_ir = BTreeMap::new();
    all_ir.insert(factory_id, vec![Statement::Expr(var("noop"))]);
    all_ir.insert(
        child_id,
        vec![
            Statement::Let {
                name: "http".into(),
                value: member(call("importDefault", int(530)), "HTTP"),
                kind: VarKind::Const,
            },
            Statement::Return(Some(var("http"))),
        ],
    );
    let reg = empty_registry_with_factory(factory_id, 10, vec![530], &[(530, "HTTPUtils")]);
    let mut children = BTreeMap::new();
    children.insert(factory_id, vec![child_id]);
    hoist(&mut all_ir, &reg, &children);

    let factory = all_ir.get(&factory_id).unwrap();
    assert!(
        matches!(
            &factory[0],
            Statement::Let {
                value: Expression::Call { .. },
                ..
            }
        ),
        "factory should start with injected loader binding: {:?}",
        factory[0]
    );
    let child = all_ir.get(&child_id).unwrap();
    match &child[0] {
        Statement::Let {
            value: Expression::Member { object, .. },
            ..
        } => {
            assert!(
                matches!(object.as_ref(), Expression::Value(Value::Variable(_))),
                "importDefault(530) should be rewritten to a binding, got {object:?}"
            );
        }
        other => panic!("unexpected child stmt: {other:?}"),
    }
}

#[test]
fn mixed_require_and_import_default_stay_distinct() {
    let factory_id = 1u32;
    let mut all_ir = BTreeMap::new();
    all_ir.insert(
        factory_id,
        vec![
            Statement::Expr(call("require", int(530))),
            Statement::Expr(member(call("importDefault", int(530)), "foo")),
        ],
    );
    let reg = empty_registry_with_factory(factory_id, 10, vec![530], &[(530, "M")]);
    hoist(&mut all_ir, &reg, &BTreeMap::new());
    let factory = all_ir.get(&factory_id).unwrap();
    let injected: Vec<_> = factory
        .iter()
        .take_while(|s| {
            matches!(
                s,
                Statement::Let {
                    value: Expression::Call { .. },
                    ..
                }
            )
        })
        .collect();
    assert_eq!(
        injected.len(),
        2,
        "expected distinct bindings for require vs importDefault, got {factory:?}"
    );
}

#[test]
fn reuses_existing_assign_binding() {
    let factory_id = 1u32;
    let mut all_ir = BTreeMap::new();
    all_ir.insert(
        factory_id,
        vec![
            Statement::Assign {
                target: AssignTarget::Variable("HTTPUtils".into()),
                value: call("require", int(530)),
            },
            Statement::Expr(member(call("require", int(530)), "post")),
        ],
    );
    let reg = empty_registry_with_factory(factory_id, 10, vec![530], &[(530, "HTTPUtils")]);
    hoist(&mut all_ir, &reg, &BTreeMap::new());
    let factory = all_ir.get(&factory_id).unwrap();
    assert!(
        matches!(
            &factory[0],
            Statement::Assign {
                value: Expression::Call { .. },
                ..
            }
        ),
        "should reuse assign binding: {:?}",
        factory[0]
    );
    match &factory[1] {
        Statement::Expr(Expression::Member { object, .. }) => {
            assert_eq!(
                object.as_ref(),
                &Expression::Value(Value::Variable("HTTPUtils".into()))
            );
        }
        other => panic!("expected rewritten member: {other:?}"),
    }
}

#[test]
fn does_not_hoist_lazy_getter_module() {
    let factory_id = 1u32;
    let getter_id = 2u32;
    let mut all_ir = BTreeMap::new();
    all_ir.insert(factory_id, vec![Statement::Expr(var("noop"))]);
    all_ir.insert(
        getter_id,
        vec![Statement::Return(Some(member(
            call("require", int(99)),
            "default",
        )))],
    );
    let reg = empty_registry_with_factory(factory_id, 10, vec![99], &[(99, "Lazy")]);
    let mut children = BTreeMap::new();
    children.insert(factory_id, vec![getter_id]);
    hoist(&mut all_ir, &reg, &children);
    let factory = all_ir.get(&factory_id).unwrap();
    assert!(
        !matches!(
            factory.first(),
            Some(Statement::Let {
                value: Expression::Call { .. },
                ..
            })
        ),
        "lazy-only module must not be injected: {factory:?}"
    );
    let getter = all_ir.get(&getter_id).unwrap();
    assert!(
        matches!(
            getter.first(),
            Some(Statement::Return(Some(Expression::Member { object, .. })))
                if matches!(object.as_ref(), Expression::Call { .. })
        ),
        "lazy getter body must keep loader call: {getter:?}"
    );
}

fn factory_has_injected_loader(factory: &[Statement]) -> bool {
    factory.iter().any(|s| {
        matches!(
            s,
            Statement::Let {
                value: Expression::Call { .. },
                ..
            }
        )
    })
}

fn is_loader_member_call(e: &Expression) -> bool {
    matches!(
        e,
        Expression::Member { object, .. } if matches!(object.as_ref(), Expression::Call { .. })
    )
}

#[test]
fn does_not_hoist_multistmt_define_property_getter() {
    let factory_id = 1u32;
    let getter_id = 2u32;
    let mut all_ir = BTreeMap::new();
    all_ir.insert(
        factory_id,
        vec![Statement::Expr(Expression::Call {
            callee: Box::new(Expression::Member {
                object: Box::new(var("Object")),
                property: PropertyKey::Ident("defineProperty".into()),
                optional: false,
            }),
            arguments: vec![
                var("obj"),
                Expression::Value(Value::Constant(crate::ir::Constant::String(
                    "ProgressBarAndroid".into(),
                ))),
                get_descriptor(getter_id),
            ],
        })],
    );
    all_ir.insert(
        getter_id,
        vec![
            Statement::Expr(member(call("require", int(131)), "default")),
            Statement::Return(Some(member(call("require", int(405)), "default"))),
        ],
    );
    let reg = empty_registry_with_factory(
        factory_id,
        10,
        vec![131, 405],
        &[(131, "warnOnce"), (405, "ProgressBarAndroid")],
    );
    let mut children = BTreeMap::new();
    children.insert(factory_id, vec![getter_id]);
    hoist(&mut all_ir, &reg, &children);

    let factory = all_ir.get(&factory_id).unwrap();
    assert!(
        !factory_has_injected_loader(factory),
        "multi-stmt getter must not inject eager imports: {factory:?}"
    );
    let getter = all_ir.get(&getter_id).unwrap();
    assert!(
        matches!(&getter[0], Statement::Expr(e) if is_loader_member_call(e)),
        "warnOnce require must stay in getter: {getter:?}"
    );
    assert!(
        matches!(&getter[1], Statement::Return(Some(e)) if is_loader_member_call(e)),
        "ProgressBar require must stay in getter: {getter:?}"
    );
}

#[test]
fn does_not_hoist_multistmt_object_literal_getter() {
    let factory_id = 1u32;
    let getter_id = 2u32;
    let mut all_ir = BTreeMap::new();
    all_ir.insert(
        factory_id,
        vec![Statement::Let {
            name: "obj".into(),
            value: get_descriptor(getter_id),
            kind: VarKind::Const,
        }],
    );
    all_ir.insert(
        getter_id,
        vec![
            Statement::Expr(call("require", int(131))),
            Statement::Return(Some(member(call("require", int(405)), "default"))),
        ],
    );
    let reg = empty_registry_with_factory(
        factory_id,
        10,
        vec![131, 405],
        &[(131, "warnOnce"), (405, "ProgressBarAndroid")],
    );
    let mut children = BTreeMap::new();
    children.insert(factory_id, vec![getter_id]);
    hoist(&mut all_ir, &reg, &children);
    assert!(
        !factory_has_injected_loader(all_ir.get(&factory_id).unwrap()),
        "object-literal get: must keep loaders lazy"
    );
}

#[test]
fn does_not_hoist_getter_via_descriptor_variable() {
    let factory_id = 1u32;
    let getter_id = 2u32;
    let mut all_ir = BTreeMap::new();
    all_ir.insert(
        factory_id,
        vec![
            Statement::Let {
                name: "desc".into(),
                value: get_descriptor(getter_id),
                kind: VarKind::Const,
            },
            Statement::Expr(Expression::Call {
                callee: Box::new(var("defineProperty")),
                arguments: vec![var("obj"), var("name"), var("desc")],
            }),
        ],
    );
    all_ir.insert(
        getter_id,
        vec![
            Statement::Expr(member(call("require", int(131)), "default")),
            Statement::Return(Some(member(call("require", int(405)), "default"))),
        ],
    );
    let reg = empty_registry_with_factory(
        factory_id,
        10,
        vec![131, 405],
        &[(131, "warnOnce"), (405, "ProgressBarAndroid")],
    );
    let mut children = BTreeMap::new();
    children.insert(factory_id, vec![getter_id]);
    hoist(&mut all_ir, &reg, &children);
    assert!(
        !factory_has_injected_loader(all_ir.get(&factory_id).unwrap()),
        "descriptor-var get: must keep loaders lazy"
    );
}

#[test]
fn does_not_reuse_child_param_as_hoist_binding() {
    let factory_id = 1u32;
    let child_id = 2u32;
    let mut all_ir = BTreeMap::new();
    all_ir.insert(
        factory_id,
        vec![Statement::Expr(member(
            call("importDefault", int(530)),
            "HTTP",
        ))],
    );
    all_ir.insert(child_id, vec![Statement::Expr(var("noop"))]);
    let reg = empty_registry_with_factory(factory_id, 10, vec![530], &[(530, "HTTPUtils")]);
    let mut children = BTreeMap::new();
    children.insert(factory_id, vec![child_id]);
    let mut params = BTreeMap::new();
    params.insert(child_id, vec!["HTTPUtils".into()]);
    hoist_module_loaders(&mut all_ir, &reg, &children, &params);

    let factory = all_ir.get(&factory_id).unwrap();
    match &factory[0] {
        Statement::Let { name, .. } => {
            assert_ne!(
                name, "HTTPUtils",
                "must not shadow child param HTTPUtils, got {factory:?}"
            );
        }
        other => panic!("expected injected binding, got {other:?}"),
    }
}

#[test]
fn does_not_treat_sibling_factory_as_descendant() {
    use crate::analysis::metro::registry::{FactoryRoles, MetroModule};
    use std::collections::HashMap;

    let factory_a = 1u32;
    let factory_b = 2u32;
    let mut all_ir = BTreeMap::new();
    all_ir.insert(factory_a, vec![Statement::Expr(var("noop"))]);
    all_ir.insert(
        factory_b,
        vec![Statement::Expr(member(call("require", int(99)), "default"))],
    );
    let mut reg = empty_registry_with_factory(factory_a, 10, vec![], &[]);
    let sibling = MetroModule {
        module_id: 20,
        function_id: factory_b,
        name: Some("Sibling".into()),
        dependencies: vec![99],
        exports: HashMap::new(),
        roles: FactoryRoles::from_param_count(7),
    };
    reg.function_to_module.insert(factory_b, 20);
    reg.factories.insert(factory_b, sibling.clone());
    reg.modules.insert(20, sibling);
    // Exotic: parent_function edge from B → A. B is still a Metro factory.
    let mut children = BTreeMap::new();
    children.insert(factory_a, vec![factory_b]);
    hoist(&mut all_ir, &reg, &children);

    let a = all_ir.get(&factory_a).unwrap();
    assert!(
        !factory_has_injected_loader(a),
        "A must not hoist B's require: {a:?}"
    );
    // B is its own factory: it may hoist internally, but A's body stays a noop.
    assert!(
        matches!(&a[0], Statement::Expr(e) if matches!(e, Expression::Value(Value::Variable(n)) if n == "noop")),
        "A must remain its own body, got {a:?}"
    );
}
