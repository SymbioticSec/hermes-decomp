use super::define_property::{
    infer_name_from_all_define_properties, infer_name_from_define_property,
};
use super::is_meaningful_require_name;
use crate::analysis::metro::detection::is_meaningful_name;
use crate::analysis::metro::registry::FactoryRoles;
use crate::ir::{target_to_key, Binding, Expression, PropertyKey, Statement, Value};
use std::collections::BTreeMap;
use std::collections::HashMap;

// A module name and whether it is the function name of the module's default
// export, the one source that identifies the module itself: a Babel helper
// module is `module.exports = function _slicedToArray`, and fourteen copies
// of it are all `_slicedToArray`.
pub(super) struct InferredName {
    pub name: String,
    pub from_default_export: bool,
}

pub(super) fn infer_module_name_from_stmts(
    stmts: &[Statement],
    functions: &BTreeMap<u32, Vec<Statement>>,
    visited: &mut std::collections::HashSet<u32>,
) -> Option<InferredName> {
    // Pre-pass: collect variable definitions for descriptor/value lookup
    let mut var_defs: HashMap<String, &Expression> = HashMap::new();
    for stmt in stmts {
        match stmt {
            Statement::Let { name, value, .. } => {
                var_defs.insert(name.clone(), value);
            }
            Statement::Assign { target, value } => {
                if let Some(name) = target_to_key(target) {
                    var_defs.insert(name, value);
                }
            }
            _ => {}
        }
    }

    // The default export's own function name comes first, before any
    // export key or incidental function: `exports.default = function toDate`
    // named its module `_typeof` after the helper declared above it.
    for stmt in stmts {
        let Statement::Assign { target, value } = stmt else {
            continue;
        };
        if !is_default_export_target(target) {
            continue;
        }
        let function_name = match value {
            Expression::Function { name: Some(n), .. } => Some(n.clone()),
            Expression::Value(Value::Binding(Binding::Variable(v))) => match var_defs.get(v) {
                Some(Expression::Function { name: Some(n), .. }) => Some(n.clone()),
                _ => None,
            },
            _ => None,
        };
        if let Some(n) = function_name {
            if is_meaningful_name(&n) && is_meaningful_require_name(&n) {
                return Some(InferredName {
                    name: n,
                    from_default_export: true,
                });
            }
        }
    }

    infer_module_name_from_stmts_heuristic(stmts, functions, visited, &var_defs).map(|name| {
        InferredName {
            name,
            from_default_export: false,
        }
    })
}

// The function name of the module's default export, if it has one.
pub(super) fn default_export_function_name(stmts: &[Statement]) -> Option<String> {
    let mut var_defs: HashMap<String, &Expression> = HashMap::new();
    for stmt in stmts {
        match stmt {
            Statement::Let { name, value, .. } => {
                var_defs.insert(name.clone(), value);
            }
            Statement::Assign { target, value } => {
                if let Some(name) = target_to_key(target) {
                    var_defs.insert(name, value);
                }
            }
            _ => {}
        }
    }
    for stmt in stmts {
        let Statement::Assign { target, value } = stmt else {
            continue;
        };
        if !is_default_export_target(target) {
            continue;
        }
        let function_name = match value {
            Expression::Function { name: Some(n), .. } => Some(n.clone()),
            Expression::Value(Value::Binding(Binding::Variable(v))) => match var_defs.get(v) {
                Some(Expression::Function { name: Some(n), .. }) => Some(n.clone()),
                _ => None,
            },
            _ => None,
        };
        if let Some(n) = function_name {
            if is_meaningful_name(&n) && is_meaningful_require_name(&n) {
                return Some(n);
            }
        }
    }
    None
}

// `module.exports = …` or `exports.default = …`, under any spelling of the
// factory parameters.
fn is_default_export_target(target: &crate::ir::AssignTarget) -> bool {
    let crate::ir::AssignTarget::Member { object, property } = target else {
        return false;
    };
    let (is_module, is_exports) = match object {
        Expression::Value(Value::Binding(Binding::Variable(n))) => (
            FactoryRoles::matches_module_name(n),
            FactoryRoles::matches_exports_name(n),
        ),
        Expression::Value(Value::Parameter(idx)) => (
            FactoryRoles::is_module_idx(*idx),
            FactoryRoles::is_exports_idx(*idx),
        ),
        _ => (false, false),
    };
    (is_module && property == "exports") || (is_exports && property == "default")
}

fn infer_module_name_from_stmts_heuristic(
    stmts: &[Statement],
    functions: &BTreeMap<u32, Vec<Statement>>,
    visited: &mut std::collections::HashSet<u32>,
    var_defs: &HashMap<String, &Expression>,
) -> Option<String> {
    for stmt in stmts {
        match stmt {
            Statement::Assign { target, value } => {
                let is_export = match target {
                    crate::ir::AssignTarget::Binding(Binding::Variable(n)) => {
                        FactoryRoles::matches_exports_name(n)
                    }
                    crate::ir::AssignTarget::Member { object, .. } => match object {
                        Expression::Value(Value::Binding(Binding::Variable(n))) => {
                            FactoryRoles::matches_module_name(n)
                                || FactoryRoles::matches_exports_name(n)
                        }
                        Expression::Value(Value::Parameter(idx)) => {
                            FactoryRoles::is_module_idx(*idx) || FactoryRoles::is_exports_idx(*idx)
                        }
                        _ => false,
                    },
                    _ => false,
                };

                if is_export {
                    if let Some(name) = infer_from_expr(value, functions, visited) {
                        return Some(name);
                    }
                    if let crate::ir::AssignTarget::Member { property, .. } = target {
                        if property == "default" {
                            if let Expression::Value(Value::Binding(Binding::Variable(v))) = value {
                                if is_meaningful_name(v) && is_meaningful_require_name(v) {
                                    return Some(v.clone());
                                }
                            }
                        }
                    }
                    if let crate::ir::AssignTarget::Member { property, .. } = target {
                        if property != "default"
                            && property != "exports"
                            && property != "__esModule"
                            && is_meaningful_name(property)
                        {
                            return Some(property.clone());
                        }
                    }
                }
                // A value stored anywhere else names nothing: `_typeof = function
                // _typeof` above the real export, `hooks` inside moment's wrapper.
            }
            Statement::Expr(expr) => {
                if let Some(name) =
                    infer_name_from_define_property(expr, var_defs, functions, visited)
                {
                    return Some(name);
                }
                // A bare call names nothing: `registerAsset({…})` in every
                // packaged asset module gave 3 030 modules one name.
            }
            _ => {}
        }
    }

    // Detect __exportStar(require(dep), exports)
    for stmt in stmts {
        let call = match stmt {
            Statement::Expr(e) => Some(e),
            Statement::Assign { value, .. } => Some(value),
            _ => None,
        };
        if let Some(Expression::Call { callee, arguments }) = call {
            let is_export_star = match &**callee {
                Expression::Value(Value::Binding(Binding::Variable(n))) => {
                    n.contains("exportStar") || n.contains("__export")
                }
                _ => false,
            };
            if is_export_star && !arguments.is_empty() {
                if let Some(name) = infer_from_expr(&arguments[0], functions, visited) {
                    return Some(name);
                }
            }
        }
    }

    // Try naming from export default { key1() {}, key2() {} }
    for stmt in stmts {
        if let Statement::Assign { target, value } = stmt {
            let is_default_export = match target {
                crate::ir::AssignTarget::Member { object, property } => {
                    property == "default"
                        && match object {
                            Expression::Value(Value::Binding(Binding::Variable(n))) => {
                                FactoryRoles::matches_exports_name(n)
                            }
                            _ => false,
                        }
                }
                _ => false,
            };
            if is_default_export {
                if let Expression::Object { properties } = value {
                    for prop in properties {
                        let key_name = match &prop.key {
                            PropertyKey::Ident(s) | PropertyKey::String(s) => Some(s.as_str()),
                            _ => None,
                        };
                        if let Some(k) = key_name {
                            if k.starts_with(|c: char| c.is_ascii_uppercase())
                                && is_meaningful_name(k)
                            {
                                return Some(k.to_string());
                            }
                        }
                    }
                    for prop in properties {
                        let key_name = match &prop.key {
                            PropertyKey::Ident(s) | PropertyKey::String(s) => Some(s.as_str()),
                            _ => None,
                        };
                        if let Some(k) = key_name {
                            if is_meaningful_name(k) && k.len() > 3 {
                                return Some(k.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    // Last resort: check for named export properties
    if let Some(name) = infer_name_from_all_define_properties(stmts) {
        return Some(name);
    }

    // No fallback on the first function declared in the body: `noop`, `n`,
    // `apply` named react, chroma and lodash after an incidental helper.

    // Try naming from requireModule.get/getEnforcing("Name")
    for stmt in stmts {
        let call_expr = match stmt {
            Statement::Let { value, .. } => Some(value),
            Statement::Assign { value, .. } => Some(value),
            Statement::Expr(e) => Some(e),
            _ => None,
        };
        if let Some(expr) = call_expr {
            if let Some(name) = extract_native_module_name(expr) {
                return Some(name);
            }
        }
    }

    // Try naming from meaningful named const exports
    for stmt in stmts {
        if let Statement::Assign { target, .. } = stmt {
            if let crate::ir::AssignTarget::Member { object, property } = target {
                let is_exports = match object {
                    Expression::Value(Value::Binding(Binding::Variable(obj_name))) => {
                        FactoryRoles::matches_exports_name(obj_name)
                    }
                    Expression::Value(Value::Parameter(idx)) => {
                        FactoryRoles::is_exports_idx(*idx) || FactoryRoles::is_module_idx(*idx)
                    }
                    _ => false,
                };
                if is_exports
                    && property != "default"
                    && property != "__esModule"
                    && is_meaningful_name(property)
                {
                    return Some(property.clone());
                }
            }
        }
    }

    // .displayName or registerComponent/registerCallableModule
    let all_exprs = collect_call_exprs_from_stmts(stmts);
    for expr in &all_exprs {
        if let Expression::Call { callee, arguments } = expr {
            if let Expression::Member { property, .. } = &**callee {
                let method = match property {
                    PropertyKey::Ident(s) | PropertyKey::String(s) => s.as_str(),
                    _ => "",
                };
                if method == "registerComponent" || method == "registerCallableModule" {
                    for arg in arguments {
                        if let Expression::Value(Value::Constant(crate::ir::Constant::String(s))) =
                            arg
                        {
                            if is_meaningful_name(s) {
                                return Some(s.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    // displayName assignments
    for stmt in stmts {
        if let Statement::Assign { target, value } = stmt {
            if let crate::ir::AssignTarget::Member { property, .. } = target {
                if property == "displayName" {
                    if let Expression::Value(Value::Constant(crate::ir::Constant::String(s))) =
                        value
                    {
                        if is_meaningful_name(s) {
                            return Some(s.clone());
                        }
                    }
                }
            }
        }
    }

    // Try naming from first meaningful Let variable name
    for stmt in stmts {
        if let Statement::Let { name, .. } = stmt {
            if is_meaningful_name(name)
                && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            {
                return Some(name.clone());
            }
        }
    }

    None
}

// Extract the name from X.getEnforcing("Name") calls (React Native native module pattern).
fn extract_native_module_name(expr: &Expression) -> Option<String> {
    use crate::ir::{Constant, Value};

    let (callee, arguments) = match expr {
        Expression::Call { callee, arguments } => (&**callee, arguments.as_slice()),
        _ => return None,
    };

    if let Expression::Member { property, .. } = callee {
        let method_name = match property {
            PropertyKey::Ident(s) | PropertyKey::String(s) => s.as_str(),
            _ => return None,
        };
        let require_pascal = method_name == "get";
        if method_name != "getEnforcing" && !require_pascal {
            return None;
        }

        for arg in arguments {
            if let Expression::Value(Value::Constant(Constant::String(s))) = arg {
                if is_meaningful_name(s) {
                    if require_pascal && !s.starts_with(|c: char| c.is_ascii_uppercase()) {
                        continue;
                    }
                    return Some(s.clone());
                }
            }
        }
    }

    None
}

// Collect all call expressions from statements, including nested blocks.
fn collect_call_exprs_from_stmts(stmts: &[Statement]) -> Vec<&Expression> {
    let mut result = Vec::new();
    for stmt in stmts {
        match stmt {
            Statement::Expr(e) => result.push(e),
            Statement::Assign { value, .. } => result.push(value),
            Statement::Let { value, .. } => result.push(value),
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                result.extend(collect_call_exprs_from_stmts(then_body));
                result.extend(collect_call_exprs_from_stmts(else_body));
            }
            Statement::While { body, .. } | Statement::Block(body) => {
                result.extend(collect_call_exprs_from_stmts(body));
            }
            Statement::For { body, .. } => {
                result.extend(collect_call_exprs_from_stmts(body));
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                finally_body,
                ..
            } => {
                result.extend(collect_call_exprs_from_stmts(try_body));
                result.extend(collect_call_exprs_from_stmts(catch_body));
                result.extend(collect_call_exprs_from_stmts(finally_body));
            }
            _ => {}
        }
    }
    result
}

pub(super) fn infer_from_expr(
    expr: &Expression,
    functions: &BTreeMap<u32, Vec<Statement>>,
    visited: &mut std::collections::HashSet<u32>,
) -> Option<String> {
    match expr {
        Expression::Function { id, name, .. } => {
            if let Some(n) = name {
                if is_meaningful_name(n) {
                    return Some(n.clone());
                }
            }
            if visited.insert(id.0) {
                if let Some(body) = functions.get(&id.0) {
                    if let Some(inner) = infer_module_name_from_stmts(body, functions, visited) {
                        return Some(inner.name);
                    }
                }
            }
            None
        }
        // A call's callee names the helper, not the value.
        Expression::Value(Value::Binding(Binding::Variable(name))) => {
            if is_meaningful_name(name) && is_meaningful_require_name(name) {
                Some(name.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}
