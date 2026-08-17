use crate::ir::{
    AssignTarget, Expression, FunctionId, ObjectProperty, PropertyKey, Statement, VarKind,
};

use super::super::detect::{loader_aliases, loader_call, resolve_module_id};
use super::super::kinds::LoaderKind;
use super::super::lazy::{
    collect_descriptor_getter_ids, is_pure_lazy_getter_body, lazy_getter_ids,
};
use super::helpers::{call, int, member, var};

#[test]
fn aliases_follow_require_copies() {
    let stmts = vec![
        Statement::Assign {
            target: AssignTarget::Variable("tmp4".into()),
            value: var("require"),
        },
        Statement::Let {
            name: "r2".into(),
            value: var("tmp4"),
            kind: VarKind::Const,
        },
    ];
    let a = loader_aliases(&stmts);
    assert_eq!(a.get("tmp4"), Some(&LoaderKind::Require));
    assert_eq!(a.get("r2"), Some(&LoaderKind::Require));
    assert_eq!(a.get("importDefault"), Some(&LoaderKind::ImportDefault));
}

#[test]
fn resolves_integer_and_dependency_map_only() {
    let deps = vec![100, 200, 300];
    assert_eq!(resolve_module_id(&int(509), &deps), Some(509));
    let dm = Expression::Member {
        object: Box::new(var("dependencyMap")),
        property: PropertyKey::Index(1),
        optional: false,
    };
    assert_eq!(resolve_module_id(&dm, &deps), Some(200));
    let arr = Expression::Member {
        object: Box::new(var("items")),
        property: PropertyKey::Index(1),
        optional: false,
    };
    assert_eq!(resolve_module_id(&arr, &deps), None);
}

#[test]
fn recognizes_import_default_loader() {
    let aliases = loader_aliases(&[]);
    let e = call("importDefault", int(530));
    assert_eq!(
        loader_call(&e, &aliases, &[]).map(|(id, k, _)| (id, k)),
        Some((530, LoaderKind::ImportDefault))
    );
}

#[test]
fn detects_lazy_getter_single_and_multi_stmt() {
    let aliases = loader_aliases(&[]);
    let getter = vec![Statement::Return(Some(member(
        call("require", int(345)),
        "default",
    )))];
    assert!(is_pure_lazy_getter_body(&getter, &aliases, &[]));
    assert_eq!(lazy_getter_ids(&getter, &aliases, &[]), vec![345]);

    let multi = vec![
        Statement::Let {
            name: "a".into(),
            value: call("require", int(345)),
            kind: VarKind::Const,
        },
        Statement::Return(Some(member(var("a"), "default"))),
    ];
    assert!(is_pure_lazy_getter_body(&multi, &aliases, &[]));

    // Eager: non-default export is not a lazy getter.
    let eager_prop = vec![
        Statement::Let {
            name: "http".into(),
            value: member(call("importDefault", int(530)), "HTTP"),
            kind: VarKind::Const,
        },
        Statement::Return(Some(var("http"))),
    ];
    assert!(!is_pure_lazy_getter_body(&eager_prop, &aliases, &[]));

    let eager = vec![
        Statement::Let {
            name: "a".into(),
            value: call("require", int(345)),
            kind: VarKind::Const,
        },
        Statement::Expr(call("sideEffect", int(0))),
        Statement::Return(Some(var("a"))),
    ];
    assert!(!is_pure_lazy_getter_body(&eager, &aliases, &[]));
}

#[test]
fn collects_getter_ids_from_get_descriptor() {
    let getter = Expression::Function {
        id: FunctionId(7),
        name: None,
        is_arrow: true,
        is_async: false,
        is_generator: false,
    };
    let desc = Expression::Object {
        properties: vec![ObjectProperty {
            key: PropertyKey::Ident("get".into()),
            value: getter,
        }],
    };
    let stmts = vec![Statement::Expr(desc)];
    let ids = collect_descriptor_getter_ids(&stmts);
    assert!(ids.contains(&7), "expected getter 7, got {ids:?}");

    let aliased = vec![
        Statement::Let {
            name: "g".into(),
            value: Expression::Function {
                id: FunctionId(8),
                name: None,
                is_arrow: true,
                is_async: false,
                is_generator: false,
            },
            kind: VarKind::Const,
        },
        Statement::Expr(Expression::Object {
            properties: vec![ObjectProperty {
                key: PropertyKey::String("get".into()),
                value: var("g"),
            }],
        }),
    ];
    let ids = collect_descriptor_getter_ids(&aliased);
    assert!(ids.contains(&8), "expected aliased getter 8, got {ids:?}");
}
