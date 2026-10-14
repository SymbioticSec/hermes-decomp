use crate::ir::{
    map_nested_bodies_mut, AssignTarget, Binding, Expression, ObjectProperty, PropertyKey,
    Statement, Value,
};
use std::collections::BTreeMap;

// Fold a straight-line run of `obj.prop = value` into the object `obj` holds,
// so `jsx(Tag, obj)` sees a literal. A later write of a `jsx(...)` call onto a
// key that already has a value is the slot-reuse overwrite (`children = real`
// then `children = jsx(View, {})`); the earlier value stays.
//
// A folded value moves up to where the object was built, so it must be
// available there: a value that reads a name bound after the object, or a
// value that runs code when something with effects ran in between, stays where
// it is. `request = {…}; obj = { nick }; request.body = obj; obj3 = require();
// request.rejectWithError = obj3.f()` used to print both fills inside the
// literal, above the lines that define `obj` and `obj3`.
pub(super) fn fold_prop_assignments(stmts: Vec<Statement>) -> Vec<Statement> {
    struct Tracked {
        value: Expression,
        defined_after: Vec<String>,
        effects_since: bool,
    }
    let mut objects: BTreeMap<String, Tracked> = BTreeMap::new();
    let mut out = Vec::with_capacity(stmts.len());
    // What a statement that is not a folded fill tells every tracked object.
    fn note(objects: &mut BTreeMap<String, Tracked>, stmt: &Statement) {
        let bound = match stmt {
            Statement::Let { name, .. } => Some(name.clone()),
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Variable(n)),
                ..
            } => Some(n.clone()),
            _ => None,
        };
        let effects = crate::ir::stmt_has_side_effects(stmt)
            || matches!(stmt, Statement::Assign { target, .. } if !matches!(target, AssignTarget::Binding(_)));
        for t in objects.values_mut() {
            if let Some(b) = &bound {
                t.defined_after.push(b.clone());
            }
            if effects {
                t.effects_since = true;
            }
        }
    }
    for stmt in stmts {
        match stmt {
            Statement::Let { name, value, kind } => {
                note(
                    &mut objects,
                    &Statement::Let {
                        name: name.clone(),
                        value: value.clone(),
                        kind,
                    },
                );
                if matches!(value, Expression::Object { .. }) {
                    objects.insert(
                        name.clone(),
                        Tracked {
                            value: value.clone(),
                            defined_after: Vec::new(),
                            effects_since: false,
                        },
                    );
                } else {
                    objects.remove(&name);
                }
                out.push(Statement::Let { name, value, kind });
            }
            Statement::Assign { target, value } => {
                if let Some(obj_name) = member_var_target(&target) {
                    if let Some(tracked) = objects.get(&obj_name) {
                        let Expression::Object { mut properties } = tracked.value.clone() else {
                            unreachable!("only object literals are tracked");
                        };
                        let available = !reads_any(&value, &tracked.defined_after)
                            && !(value.has_side_effects() && tracked.effects_since);
                        if let (Some(key), true) = (ident_key(&target), available) {
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
                                if let Some(t) = objects.get_mut(&obj_name) {
                                    t.value = obj.clone();
                                }
                                update_binding_value(&mut out, &obj_name, obj);
                            }
                            continue;
                        }
                    }
                }
                let stmt = Statement::Assign { target, value };
                note(&mut objects, &stmt);
                if let Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable(name)),
                    value,
                } = &stmt
                {
                    if matches!(value, Expression::Object { .. }) {
                        objects.insert(
                            name.clone(),
                            Tracked {
                                value: value.clone(),
                                defined_after: Vec::new(),
                                effects_since: false,
                            },
                        );
                    } else {
                        objects.remove(name);
                    }
                }
                out.push(stmt);
            }
            other => {
                let mut s = other;
                note(&mut objects, &s);
                map_nested_bodies_mut(&mut s, fold_prop_assignments);
                out.push(s);
            }
        }
    }
    out
}

fn reads_any(expr: &Expression, names: &[String]) -> bool {
    use crate::ir::Visitor;
    struct C<'n>(&'n [String], bool);
    impl<'a> Visitor<'a> for C<'_> {
        fn visit_expression(&mut self, e: &'a Expression) {
            if let Expression::Value(Value::Binding(Binding::Variable(n))) = e {
                if self.0.contains(n) {
                    self.1 = true;
                }
            }
            self.walk_expression(e);
        }
    }
    let mut c = C(names, false);
    c.visit_expression(expr);
    c.1
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
