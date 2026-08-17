// Loader recognition: aliases, dep-map args, call matching, use collection.

use std::collections::HashMap;

use crate::ir::{AssignTarget, Constant, Expression, PropertyKey, Statement, Value, Visitor};

use super::kinds::{LoaderKind, LOADER_NAMES};

// Names that hold a loader within a body: seed loaders plus pure variable copies,
// mapped to the seed LoaderKind they alias.
pub(super) fn loader_aliases(stmts: &[Statement]) -> HashMap<String, LoaderKind> {
    let mut map: HashMap<String, LoaderKind> = HashMap::new();
    for &name in LOADER_NAMES {
        if let Some(k) = LoaderKind::from_seed(name) {
            map.insert(name.to_string(), k);
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for s in stmts {
            let (name, value) = match s {
                Statement::Let { name, value, .. } => (name, value),
                Statement::Assign {
                    target: AssignTarget::Variable(name),
                    value,
                } => (name, value),
                _ => continue,
            };
            if let Expression::Value(Value::Variable(v)) = value {
                if let Some(&kind) = map.get(v) {
                    if map.insert(name.clone(), kind).is_none() {
                        changed = true;
                    }
                }
            }
        }
    }
    map
}

fn is_depmap_name(name: &str) -> bool {
    name == "dependencyMap"
        || name == "_dependencyMap"
        || name == "deps"
        || name.starts_with("dependencyMap")
        || (name.starts_with("deps")
            && name.len() > 4
            && name[4..].chars().all(|c| c.is_ascii_digit()))
}

fn is_depmap_object(expr: &Expression) -> bool {
    match expr {
        Expression::Value(Value::Variable(name)) => is_depmap_name(name),
        _ => false,
    }
}

// Resolve a loader call argument to an absolute module id: `loader(1234)` -> 1234,
// `loader(dependencyMap[i])` -> deps[i]. Non-depmap member indices are rejected.
pub(super) fn resolve_module_id(arg: &Expression, deps: &[u32]) -> Option<u32> {
    match arg {
        Expression::Value(Value::Constant(Constant::Integer(n))) if *n >= 0 => Some(*n as u32),
        Expression::Member {
            object,
            property: PropertyKey::Index(i),
            ..
        } if *i >= 0 => {
            if is_depmap_object(object) {
                deps.get(*i as usize).copied()
            } else {
                None
            }
        }
        Expression::Member {
            object,
            property: PropertyKey::Computed(k),
            ..
        } => {
            if !is_depmap_object(object) {
                return None;
            }
            if let Expression::Value(Value::Constant(Constant::Integer(i))) = k.as_ref() {
                if *i >= 0 {
                    return deps.get(*i as usize).copied();
                }
            }
            None
        }
        _ => None,
    }
}

// If `expr` is `LOADER(id)` with a recognized loader callee, return (id, kind, callee).
pub(super) fn loader_call<'a>(
    expr: &'a Expression,
    aliases: &HashMap<String, LoaderKind>,
    deps: &[u32],
) -> Option<(u32, LoaderKind, &'a Expression)> {
    let Expression::Call { callee, arguments } = expr else {
        return None;
    };
    let name = match callee.as_ref() {
        Expression::Value(Value::Variable(n)) => n.as_str(),
        _ => return None,
    };
    let kind = *aliases.get(name)?;
    let id_arg = match arguments.first() {
        Some(Expression::Value(Value::Constant(Constant::Undefined))) => arguments.get(1),
        other => other,
    }?;
    let id = resolve_module_id(id_arg, deps)?;
    Some((id, kind, callee.as_ref()))
}

// Eager loader uses in a body: (module id, kind, callee).
pub(super) fn loader_uses(
    stmts: &[Statement],
    aliases: &HashMap<String, LoaderKind>,
    deps: &[u32],
) -> Vec<(u32, LoaderKind, Expression)> {
    struct C<'a> {
        aliases: &'a HashMap<String, LoaderKind>,
        deps: &'a [u32],
        out: Vec<(u32, LoaderKind, Expression)>,
    }
    impl Visitor<'_> for C<'_> {
        fn visit_expression(&mut self, e: &Expression) {
            if let Some((id, kind, callee)) = loader_call(e, self.aliases, self.deps) {
                self.out.push((id, kind, callee.clone()));
            }
            self.walk_expression(e);
        }
    }
    let mut c = C {
        aliases,
        deps,
        out: Vec::new(),
    };
    for s in stmts {
        c.visit_statement(s);
    }
    c.out
}

pub(super) fn collect_loader_ids(
    expr: &Expression,
    aliases: &HashMap<String, LoaderKind>,
    deps: &[u32],
    out: &mut Vec<u32>,
) {
    struct C<'a> {
        aliases: &'a HashMap<String, LoaderKind>,
        deps: &'a [u32],
        out: &'a mut Vec<u32>,
    }
    impl Visitor<'_> for C<'_> {
        fn visit_expression(&mut self, e: &Expression) {
            if let Some((id, _, _)) = loader_call(e, self.aliases, self.deps) {
                self.out.push(id);
            }
            self.walk_expression(e);
        }
    }
    let mut c = C {
        aliases,
        deps,
        out,
    };
    c.visit_expression(expr);
}
