// Detect React Native lazy-loader getter bodies so they are not forced eager.
//
// Primary gate: a function used as a `get:` descriptor value (any statement
// count). Body-shape (`is_pure_lazy_getter_body`) is only a fallback for
// getters we fail to attach at the use site.

use std::collections::{HashMap, HashSet};

use crate::ir::{AssignTarget, Expression, PropertyKey, Statement, Value, Visitor};

use super::detect::{collect_loader_ids, loader_call};
use super::kinds::LoaderKind;

// Whether the body is a pure lazy loader getter (only loads one module, no other work).
pub(super) fn is_pure_lazy_getter_body(
    stmts: &[Statement],
    aliases: &HashMap<String, LoaderKind>,
    deps: &[u32],
) -> bool {
    lazy_getter_ids(stmts, aliases, deps).len() == 1
        && body_is_only_loader_plumbing(stmts, aliases, deps)
}

// Module ids loaded by a pure lazy-getter-shaped body (0 or 1 typically).
pub(super) fn lazy_getter_ids(
    stmts: &[Statement],
    aliases: &HashMap<String, LoaderKind>,
    deps: &[u32],
) -> Vec<u32> {
    if !body_is_only_loader_plumbing(stmts, aliases, deps) {
        return Vec::new();
    }
    let mut ids = Vec::new();
    for s in stmts {
        if matches!(s, Statement::Comment(_)) {
            continue;
        }
        match s {
            Statement::Return(Some(e))
            | Statement::Let { value: e, .. }
            | Statement::Assign { value: e, .. } => {
                collect_loader_ids(e, aliases, deps, &mut ids);
            }
            _ => {}
        }
    }
    ids.sort_unstable();
    ids.dedup();
    ids
}

// Body is only comments + pure lets/assigns + a final return, no control flow /
// side calls beyond a single-module loader chain (RN lazy getter shapes).
fn body_is_only_loader_plumbing(
    stmts: &[Statement],
    aliases: &HashMap<String, LoaderKind>,
    deps: &[u32],
) -> bool {
    let real: Vec<&Statement> = stmts
        .iter()
        .filter(|s| !matches!(s, Statement::Comment(_)))
        .collect();
    if real.is_empty() || real.len() > 4 {
        return false;
    }
    let Some(Statement::Return(Some(_))) = real.last().copied() else {
        return false;
    };
    for s in &real[..real.len() - 1] {
        match s {
            Statement::Let { value, .. }
            | Statement::Assign {
                target: AssignTarget::Variable(_),
                value,
            } => {
                if !is_lazy_plumbing_value(value, aliases, deps) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    // Return may be loader, loader.default, var, or var.default.
    if let Statement::Return(Some(e)) = real[real.len() - 1] {
        if !is_lazy_plumbing_value(e, aliases, deps) && !is_var_or_default_of_var(e) {
            return false;
        }
    }
    let mut ids = Vec::new();
    for s in real {
        match s {
            Statement::Return(Some(e))
            | Statement::Let { value: e, .. }
            | Statement::Assign { value: e, .. } => {
                collect_loader_ids(e, aliases, deps, &mut ids);
            }
            _ => {}
        }
    }
    ids.sort_unstable();
    ids.dedup();
    ids.len() == 1
}

fn is_default_key(property: &PropertyKey) -> bool {
    match property {
        PropertyKey::Ident(p) | PropertyKey::String(p) => p == "default",
        _ => false,
    }
}

fn is_var_or_default_of_var(expr: &Expression) -> bool {
    match expr {
        Expression::Value(Value::Variable(_)) => true,
        Expression::Member {
            object, property, ..
        } if is_default_key(property) => {
            matches!(object.as_ref(), Expression::Value(Value::Variable(_)))
        }
        _ => false,
    }
}

// Function ids that appear as a `get:` descriptor value in `stmts`.
// Walks object literals (covers inline `Object.defineProperty` descriptors)
// and hops one step through `get: var` when `var` is bound to a Function.
pub(super) fn collect_descriptor_getter_ids(stmts: &[Statement]) -> HashSet<u32> {
    let mut var_fns = HashMap::new();
    collect_function_bindings(stmts, &mut var_fns);
    let mut ids = HashSet::new();
    struct C<'a> {
        var_fns: &'a HashMap<String, u32>,
        out: &'a mut HashSet<u32>,
    }
    impl Visitor<'_> for C<'_> {
        fn visit_expression(&mut self, e: &Expression) {
            if let Expression::Object { properties } = e {
                for p in properties {
                    if is_get_key(&p.key) {
                        if let Some(id) = function_id_of(&p.value, self.var_fns) {
                            self.out.insert(id);
                        }
                    }
                }
            }
            self.walk_expression(e);
        }
    }
    let mut c = C {
        var_fns: &var_fns,
        out: &mut ids,
    };
    for s in stmts {
        c.visit_statement(s);
    }
    ids
}

fn is_get_key(key: &PropertyKey) -> bool {
    match key {
        PropertyKey::Ident(p) | PropertyKey::String(p) => p == "get",
        _ => false,
    }
}

fn function_id_of(expr: &Expression, var_fns: &HashMap<String, u32>) -> Option<u32> {
    match expr {
        Expression::Function { id, .. } => Some(id.0),
        Expression::Value(Value::Variable(n)) => var_fns.get(n).copied(),
        _ => None,
    }
}

fn collect_function_bindings(stmts: &[Statement], out: &mut HashMap<String, u32>) {
    for s in stmts {
        match s {
            Statement::Let { name, value, .. } => {
                if let Expression::Function { id, .. } = value {
                    out.insert(name.clone(), id.0);
                }
            }
            Statement::Assign {
                target: AssignTarget::Variable(name),
                value,
            } => {
                if let Expression::Function { id, .. } = value {
                    out.insert(name.clone(), id.0);
                }
            }
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                collect_function_bindings(then_body, out);
                collect_function_bindings(else_body, out);
            }
            Statement::While { body, .. }
            | Statement::DoWhile { body, .. }
            | Statement::Block(body) => collect_function_bindings(body, out),
            Statement::For { init, body, update, .. } => {
                if let Some(i) = init {
                    collect_function_bindings(std::slice::from_ref(i.as_ref()), out);
                }
                if let Some(u) = update {
                    collect_function_bindings(std::slice::from_ref(u.as_ref()), out);
                }
                collect_function_bindings(body, out);
            }
            Statement::ForIn { body, .. } | Statement::ForOf { body, .. } => {
                collect_function_bindings(body, out);
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                finally_body,
                ..
            } => {
                collect_function_bindings(try_body, out);
                collect_function_bindings(catch_body, out);
                collect_function_bindings(finally_body, out);
            }
            Statement::Switch {
                cases, default, ..
            } => {
                for (_, body) in cases {
                    collect_function_bindings(body, out);
                }
                if let Some(d) = default {
                    collect_function_bindings(d, out);
                }
            }
            _ => {}
        }
    }
}

// Values allowed in a lazy getter: loader(N), loader(N).default, interop(loader),
// a local, or local.default — never loader(N).SomeExport (that's eager use).
fn is_lazy_plumbing_value(
    expr: &Expression,
    aliases: &HashMap<String, LoaderKind>,
    deps: &[u32],
) -> bool {
    if loader_call(expr, aliases, deps).is_some() {
        return true;
    }
    match expr {
        Expression::Value(Value::Variable(_)) => true,
        Expression::Member {
            object, property, ..
        } if is_default_key(property) => is_lazy_plumbing_value(object, aliases, deps),
        Expression::Call { callee, arguments } => {
            // Allow interop wrappers around a loader: _interopRequireDefault(require(N))
            let callee_ok = match callee.as_ref() {
                Expression::Value(Value::Variable(n)) => {
                    n.contains("interop")
                        || n == "_interopRequireDefault"
                        || n == "_interopDefault"
                }
                _ => false,
            };
            if !callee_ok || arguments.len() != 1 {
                return false;
            }
            is_lazy_plumbing_value(&arguments[0], aliases, deps)
        }
        _ => false,
    }
}
