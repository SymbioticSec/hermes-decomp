use crate::ir::{
    map_nested_bodies_mut, AssignTarget, Binding, Expression, ObjectProperty, PropertyKey,
    Statement, Value,
};
use std::collections::BTreeMap;

// Fold a straight-line run of `obj.prop = value` into the object `obj` holds,
// so `jsx(Tag, obj)` sees a literal. A later write of a `jsx(...)` call onto a
// key that already has a value is the slot-reuse overwrite (`children = real`
// then `children = jsx(View, {})`); the earlier value stays.
pub(super) fn fold_prop_assignments(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut objects: BTreeMap<String, Expression> = BTreeMap::new();
    let mut out = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        match stmt {
            Statement::Let { name, value, kind } => {
                if matches!(value, Expression::Object { .. }) {
                    objects.insert(name.clone(), value.clone());
                } else {
                    objects.remove(&name);
                }
                out.push(Statement::Let { name, value, kind });
            }
            Statement::Assign { target, value } => {
                if let Some(obj_name) = member_var_target(&target) {
                    if let Some(Expression::Object { mut properties }) =
                        objects.get(&obj_name).cloned()
                    {
                        if let Some(key) = ident_key(&target) {
                            let later_is_jsx = matches!(&value, Expression::Call { callee, .. } if super::is_jsx_call(callee));
                            let already = properties.iter().any(|p| prop_key_eq(&p.key, &key));
                            if !(already && later_is_jsx) {
                                if let Some(existing) =
                                    properties.iter_mut().find(|p| prop_key_eq(&p.key, &key))
                                {
                                    existing.value = value;
                                } else {
                                    properties.push(ObjectProperty { key, value });
                                }
                                let obj = Expression::Object { properties };
                                objects.insert(obj_name.clone(), obj.clone());
                                update_binding_value(&mut out, &obj_name, obj);
                            }
                            continue;
                        }
                    }
                }
                if let AssignTarget::Binding(Binding::Variable(name)) = &target {
                    if matches!(value, Expression::Object { .. }) {
                        objects.insert(name.clone(), value.clone());
                    } else {
                        objects.remove(name);
                    }
                }
                out.push(Statement::Assign { target, value });
            }
            other => {
                let mut s = other;
                map_nested_bodies_mut(&mut s, fold_prop_assignments);
                out.push(s);
            }
        }
    }
    out
}

fn member_var_target(target: &AssignTarget) -> Option<String> {
    if let AssignTarget::Member { object, .. } = target {
        if let Expression::Value(Value::Binding(Binding::Variable(name))) = object {
            return Some(name.clone());
        }
    }
    None
}

fn ident_key(target: &AssignTarget) -> Option<PropertyKey> {
    if let AssignTarget::Member { property, .. } = target {
        return Some(PropertyKey::Ident(property.clone()));
    }
    None
}

fn prop_key_eq(a: &PropertyKey, b: &PropertyKey) -> bool {
    match (a, b) {
        (
            PropertyKey::Ident(x) | PropertyKey::String(x),
            PropertyKey::Ident(y) | PropertyKey::String(y),
        ) => x == y,
        _ => false,
    }
}

fn update_binding_value(stmts: &mut [Statement], name: &str, value: Expression) {
    for stmt in stmts.iter_mut().rev() {
        match stmt {
            Statement::Let {
                name: n, value: v, ..
            } if n == name => {
                *v = value;
                return;
            }
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Variable(n)),
                value: v,
            } if n == name => {
                *v = value;
                return;
            }
            _ => {}
        }
    }
}
