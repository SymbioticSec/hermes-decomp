use crate::analysis::metro::registry::MetroRegistry;
use crate::ir::{Expression, PropertyKey, Value};
use std::collections::HashMap;

use super::traversal::Definition;

// Index of function names to candidate function IDs.
// We keep all IDs because names are often duplicated in production bundles.
pub type FunctionNameIndex = HashMap<String, Vec<u32>>;

pub(super) fn resolve_callee(
    callee: &Expression,
    defs: &HashMap<String, Definition>,
    metro_registry: &MetroRegistry,
    func_name_index: &FunctionNameIndex,
) -> Option<u32> {
    match callee {
        Expression::Value(Value::Variable(name)) => {
            if let Some(def) = defs.get(name) {
                match def {
                    Definition::Function(fid) => return Some(*fid),
                    Definition::Module(mod_id) => {
                        if let Some(module) = metro_registry.get_module(*mod_id) {
                            if let Some(fid) = module.exports.get("default") {
                                return Some(*fid);
                            }
                        }
                    }
                    _ => {}
                }
            }
            // Fallback: check if variable name matches a known function name
            if let Some(fid) = resolve_unique_by_name(name, func_name_index) {
                return Some(fid);
            }
            None
        }
        Expression::Value(Value::Register(r)) => {
            let r_name = format!("r{r}");
            if let Some(def) = defs.get(&r_name) {
                match def {
                    Definition::Function(fid) => return Some(*fid),
                    Definition::Module(mod_id) => {
                        if let Some(module) = metro_registry.get_module(*mod_id) {
                            if let Some(fid) = module.exports.get("default") {
                                return Some(*fid);
                            }
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        Expression::Function { id, .. } => Some(id.0),

        // Handle member access: obj.methodName()
        Expression::Member {
            object, property, ..
        } => {
            // First try to resolve via module registry
            if let Some(base_name) = get_base_name(object) {
                if let Some(def) = defs.get(&base_name) {
                    if let Definition::Module(mod_id) = def {
                        let prop_name = match property {
                            PropertyKey::String(s) | PropertyKey::Ident(s) => Some(s.as_str()),
                            _ => None,
                        };
                        if let Some(prop_name) = prop_name {
                            if let Some(module) = metro_registry.get_module(*mod_id) {
                                if let Some(fid) = module.exports.get(prop_name) {
                                    return Some(*fid);
                                }
                            }
                        }
                    }
                }
            }
            // Fallback: check if property name matches a known function name
            let prop_name = match property {
                PropertyKey::String(s) | PropertyKey::Ident(s) => Some(s.as_str()),
                _ => None,
            };
            if let Some(name) = prop_name {
                if let Some(fid) = resolve_unique_by_name(name, func_name_index) {
                    return Some(fid);
                }
            }
            None
        }
        _ => None,
    }
}

fn resolve_unique_by_name(name: &str, func_name_index: &FunctionNameIndex) -> Option<u32> {
    match func_name_index.get(name) {
        Some(ids) if ids.len() == 1 => ids.first().copied(),
        _ => None,
    }
}

pub(super) fn get_base_name(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Value(Value::Variable(name)) => Some(name.clone()),
        Expression::Value(Value::Register(r)) => Some(format!("r{r}")),
        _ => None,
    }
}

// Extract a name hint from a call expression's callee to name the return value or parameters.
//
// Heuristic:
// - If the function name is verb-noun (e.g., `getEmail`, `fetchUser`), we extract the noun.
// - We strip common verb prefixes ("get", "fetch", "load", etc.).
// - This helps naming variables holding the result: `var email = getEmail();`
pub(super) fn extract_name_from_callee(callee: &Expression) -> Option<String> {
    let name = match callee {
        Expression::Value(Value::Variable(name)) => name.clone(),
        Expression::Member {
            property: PropertyKey::String(prop),
            ..
        }
        | Expression::Member {
            property: PropertyKey::Ident(prop),
            ..
        } => prop.clone(),
        _ => return None,
    };

    // Strip common prefixes: get, fetch, load, read, find, create, make, build
    let prefixes = [
        "get", "fetch", "load", "read", "find", "create", "make", "build",
        "compute", "calculate",
    ];
    let lower = name.to_lowercase();

    for prefix in prefixes {
        if lower.starts_with(prefix) && name.len() > prefix.len() {
            let rest = &name[prefix.len()..];
            // Make sure next char was uppercase (camelCase) or underscore
            if let Some(stripped) = rest.strip_prefix('_') {
                return Some(stripped.to_string());
            } else if rest
                .chars()
                .next()
                .map(|c| c.is_uppercase())
                .unwrap_or(false)
            {
                // Convert first char to lowercase: Email -> email
                let mut chars = rest.chars();
                if let Some(first) = chars.next() {
                    return Some(first.to_lowercase().chain(chars).collect());
                }
            }
        }
    }

    // No prefix found, return as-is if it looks like a noun (not a verb pattern)
    None
}

// Extract the object name from a method call like `user.getName()` -> "user"
// We skip this heuristic for array/string transformation methods where the object name
// doesn't represent what the result actually is.
pub(super) fn extract_object_name_from_method_call(callee: &Expression) -> Option<String> {
    if let Expression::Member {
        object, property, ..
    } = callee
    {
        // Skip transformation methods - the object name doesn't describe the result
        let method_name = match property {
            PropertyKey::String(s) | PropertyKey::Ident(s) => Some(s.as_str()),
            _ => None,
        };

        if let Some(method) = method_name {
            if crate::constants::is_transformation_method(method) {
                return None;
            }
        }

        if let Expression::Value(Value::Variable(name)) = object.as_ref() {
            // Filter out generic names
            if !super::inference::is_generic_name(name) {
                return Some(name.clone());
            }
        }
    }
    None
}
