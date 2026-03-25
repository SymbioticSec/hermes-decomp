// Require call resolution — resolves require() calls to module IDs.

use super::{default_roles, is_dep_array_name, is_dep_array_param_idx};
use crate::analysis::metro::registry::{FactoryRoles, MetroRegistry};
use crate::ir::{Expression, PropertyKey, Value};
use std::collections::HashMap;

pub(super) fn resolve_require_module(
    expr: &Expression,
    func_id: u32,
    registry: &MetroRegistry,
    reg_params: &HashMap<String, u32>,
    reg_props: &HashMap<String, (String, u32)>,
) -> Option<u32> {
    if let Expression::Call { callee, arguments } = expr {
        let is_require = match &**callee {
            Expression::Value(Value::Variable(name)) => default_roles().is_require_param(name),
            Expression::Value(Value::Parameter(idx)) if *idx == default_roles().require_idx => true,
            _ => false,
        };

        if is_require {
            // Argument is usually at index 0 (if pure) or 1 (if call with this)
            let arg_expr = if arguments.len() == 1 {
                Some(&arguments[0])
            } else if arguments.len() == 2 {
                Some(&arguments[1])
            } else {
                None
            };

            if let Some(arg) = arg_expr {
                // Sub-case 1.1: Constant Integer
                if let Expression::Value(Value::Constant(crate::ir::Constant::Integer(id))) = arg {
                    return Some(*id as u32);
                }

                // Sub-case 1.2: Register (Dynamic Require via Dependency Array)
                let reg_name = match arg {
                    Expression::Value(Value::Register(r)) => Some(format!("r{r}")),
                    Expression::Value(Value::Variable(n)) => Some(n.clone()),
                    _ => None,
                };

                if let Some(r) = reg_name {
                    // Trace r to find the source property access
                    if let Some((base, idx)) = reg_props.get(&r) {
                        // Check if base is a parameter (argN) or named dependency array
                        let is_dep = is_dep_array_name(base, &default_roles());
                        let param_idx = if is_dep {
                            Some(4u32)
                        } else {
                            reg_params.get(base).copied().or_else(|| {
                                FactoryRoles::extract_param_index(base)
                            })
                        };

                        // Only resolve if this comes from the dependency array parameter (>= 4)
                        if let Some(p_idx) = param_idx {
                            if is_dep_array_param_idx(p_idx, &default_roles()) {
                                if let Some(module) = registry.get_module_for_function(func_id) {
                                    if (*idx as usize) < module.dependencies.len() {
                                        let mod_id = module.dependencies[*idx as usize];
                                        return Some(mod_id);
                                    }
                                }
                            }
                        }
                    }
                }

                // Sub-case 1.3: Member Expression (Direct dependency map lookup)
                if let Some(Expression::Member {
                    object,
                    property: PropertyKey::Index(idx),
                    ..
                }) = arg_expr
                {
                    let base_name = match &**object {
                        Expression::Value(Value::Register(r)) => Some(format!("r{r}")),
                        Expression::Value(Value::Variable(n)) => Some(n.clone()),
                        Expression::Value(Value::Parameter(i)) => Some(format!("arg{i}")),
                        _ => None,
                    };
                    if let Some(base) = base_name {
                        let is_dep_array = is_dep_array_name(&base, &default_roles());
                        let param_idx = if is_dep_array {
                            Some(4u32) // treat as dependency array param
                        } else {
                            FactoryRoles::extract_param_index(&base)
                                .or_else(|| reg_params.get(&base).copied())
                        };
                        if let Some(p_idx) = param_idx {
                            if is_dep_array_param_idx(p_idx, &default_roles()) {
                                if let Some(module) = registry.get_module_for_function(func_id) {
                                    if (*idx as usize) < module.dependencies.len() {
                                        return Some(module.dependencies[*idx as usize]);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

// Extract module ID from a require() call expression.
// Matches patterns like:
// - require(123)
// - require(arg1, 123)  // with this pointer
pub(super) fn extract_require_module_id(expr: &Expression) -> Option<u32> {
    match expr {
        // Direct call: require(n) or _interopDefault(require(n))
        Expression::Call { callee, arguments } => {
            let is_require = is_require_callee(callee);
            let is_interop = is_interop_callee(callee);

            if is_require && arguments.len() == 1 {
                extract_module_id_from_arg(&arguments[0])
            } else if is_require && arguments.len() == 2 {
                extract_module_id_from_arg(&arguments[1])
            } else if is_interop && !arguments.is_empty() {
                for arg in arguments {
                    if let Some(id) = extract_require_module_id(arg) {
                        return Some(id);
                    }
                }
                None
            } else {
                None
            }
        }
        _ => None,
    }
}

// Check if callee is a require function
fn is_require_callee(callee: &Expression) -> bool {
    match callee {
        Expression::Value(Value::Variable(name)) => {
            default_roles().is_require_param(name) || name.starts_with("require_")
        }
        Expression::Value(Value::Parameter(idx)) => *idx == default_roles().require_idx,
        _ => false,
    }
}

// Check if callee is an interop default wrapper
fn is_interop_callee(callee: &Expression) -> bool {
    match callee {
        Expression::Value(Value::Variable(name)) => {
            name.contains("interop") || name.contains("_interop")
        }
        _ => false,
    }
}

// Extract module ID from a call argument
fn extract_module_id_from_arg(arg: &Expression) -> Option<u32> {
    match arg {
        Expression::Value(Value::Constant(crate::ir::Constant::Integer(n))) => Some(*n as u32),
        Expression::Value(Value::Constant(crate::ir::Constant::String(s))) => s.parse::<u32>().ok(),
        Expression::Value(Value::Parameter(_idx)) => None,
        Expression::Member {
            object, property, ..
        } => {
            if let Expression::Value(Value::Variable(name)) = object.as_ref() {
                if is_dep_array_name(name, &default_roles()) {
                    if let crate::ir::PropertyKey::Index(_idx) = property {
                        return None;
                    }
                }
            }
            None
        }
        _ => None,
    }
}
