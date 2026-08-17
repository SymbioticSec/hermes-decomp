use std::collections::HashMap;

use crate::analysis::metro::registry::{FactoryRoles, MetroModule, MetroRegistry};
use crate::ir::{Constant, Expression, PropertyKey, Value};

pub(super) fn int(n: i32) -> Expression {
    Expression::Value(Value::Constant(Constant::Integer(n)))
}

pub(super) fn var(n: &str) -> Expression {
    Expression::Value(Value::Variable(n.to_string()))
}

pub(super) fn call(callee: &str, arg: Expression) -> Expression {
    Expression::Call {
        callee: Box::new(var(callee)),
        arguments: vec![arg],
    }
}

pub(super) fn member(obj: Expression, prop: &str) -> Expression {
    Expression::Member {
        object: Box::new(obj),
        property: PropertyKey::Ident(prop.into()),
        optional: false,
    }
}

pub(super) fn empty_registry_with_factory(
    factory_id: u32,
    module_id: u32,
    deps: Vec<u32>,
    dep_names: &[(u32, &str)],
) -> MetroRegistry {
    let mut reg = MetroRegistry::new();
    let module = MetroModule {
        module_id,
        function_id: factory_id,
        name: Some("Factory".into()),
        dependencies: deps,
        exports: HashMap::new(),
        roles: FactoryRoles::from_param_count(7),
    };
    reg.function_to_module.insert(factory_id, module_id);
    reg.factories.insert(factory_id, module.clone());
    reg.modules.insert(module_id, module);
    for &(id, name) in dep_names {
        reg.modules.insert(
            id,
            MetroModule {
                module_id: id,
                function_id: 0,
                name: Some(name.into()),
                dependencies: vec![],
                exports: HashMap::new(),
                roles: FactoryRoles::standard(),
            },
        );
    }
    reg
}
