// Module name propagation — traces require() calls to name Metro modules and variables.

mod define_property;
mod inference;
mod phases;
mod require_resolution;

use super::detection::is_meaningful_name;
use super::registry::{FactoryRoles, MetroRegistry};
use crate::analysis::ClosureContext;
use crate::ir::{target_to_key, Expression, Statement, Value};
use std::collections::HashMap;
use std::collections::BTreeMap;

use inference::infer_module_name_from_stmts;
use phases::{propagate_module_names_to_closures, propagate_reexport_names, reverse_require_naming};
use require_resolution::resolve_require_module;

// Maximum iterations for dependency-chain module naming.
// Convergence typically occurs in 2-3 iterations; 20 guarantees termination.
const MAX_MODULE_NAME_ITERATIONS: usize = 20;

// Maximum depth for walking parent closure chains to find a factory function.
const MAX_PARENT_CHAIN_DEPTH: usize = 10;

pub(super) fn is_dep_array_name(name: &str, roles: &FactoryRoles) -> bool {
    roles.is_deps_param(name)
        || name == "dependencyMap"
        || name == "deps"
        || name.starts_with("dep")
}

pub(super) fn is_dep_array_param_idx(p_idx: u32, roles: &FactoryRoles) -> bool {
    if let Some(deps_idx) = roles.deps_idx {
        p_idx == deps_idx
    } else {
        // Fallback: anything above exports (index 3) is likely deps
        p_idx > roles.exports_idx
    }
}

// Default factory roles (used when we don't have a specific module context).
pub(super) fn default_roles() -> FactoryRoles {
    FactoryRoles::standard()
}

pub(super) fn is_meaningful_require_name(name: &str) -> bool {
    if name.len() < 2 { return false; }
    // Reject obviously generic names (shared core: exact matches, prefixes, *Result)
    if super::is_obviously_generic(name) { return false; }
    // Reject register-like names: r0, r123, v0, v123
    if name.starts_with('r') && name[1..].chars().all(|c| c.is_ascii_digit()) { return false; }
    if name.starts_with('v') && name[1..].chars().all(|c| c.is_ascii_digit()) { return false; }
    // Reject short generic names: obj, obj2, fn, fn2, arr, arr2
    if name.starts_with("obj") && name.len() <= 5 { return false; }
    if name.starts_with("fn") && name.len() <= 4 { return false; }
    if name.starts_with("arr") && name.len() <= 5 { return false; }
    true
}

pub fn propagate_module_names(
    functions: &mut BTreeMap<u32, Vec<Statement>>,
    registry: &mut MetroRegistry,
    closure_ctx: &mut Option<ClosureContext>,
) {
    // PHASE 0: Reverse require naming — scan all factories for `varName = require(depId)`
    // where varName is meaningful, and use it to name the referenced module.
    reverse_require_naming(functions, registry);

    // PHASE 0b: Infer names for anonymous modules based on exports/body analysis
    let mut inferred_names = BTreeMap::new();
    let mut mod_ids: Vec<_> = registry.modules.keys().copied().collect();
    mod_ids.sort();
    for mod_id in &mod_ids {
        let module = &registry.modules[mod_id];
        if module.name.is_none() {
            if let Some(factory_stmts) = functions.get(&module.function_id) {
                let mut visited = std::collections::HashSet::new();
                if let Some(name) = infer_module_name_from_stmts(factory_stmts, functions, &mut visited) {
                    inferred_names.insert(*mod_id, name);
                }
            }
        }
    }

    // Store inferred names back into the registry so downstream consumers see them
    let inferred_count = inferred_names.len();
    for (mod_id, name) in &inferred_names {
        if let Some(module) = registry.modules.get_mut(mod_id) {
            if module.name.is_none() && is_meaningful_name(name) {
                module.name = Some(name.clone());
            }
        }
    }

    // PHASE 0c: Re-export propagation — modules that simply re-export a dependency
    // inherit the name from their dependency (e.g., module 66 re-exports module 67 → named "getIteratorFn")
    let reexport_count = propagate_reexport_names(functions, registry);

    // PHASE 0d: Dependency-chain naming — thin unnamed wrappers inherit their sole dependency's name
    // This handles CJS interop wrappers and single-dep re-export modules
    let mut _dep_named = 0;
    for _ in 0..MAX_MODULE_NAME_ITERATIONS {
        let mut new_dep_names: Vec<(u32, String)> = Vec::new();
        let mut dep_mod_ids: Vec<_> = registry.modules.keys().copied().collect();
        dep_mod_ids.sort();
        for mid in &dep_mod_ids {
            let module = &registry.modules[mid];
            if module.name.is_some() { continue; }
            let Some(stmts) = functions.get(&module.function_id) else { continue };
            // Only thin wrappers: 1 dep (≤15 stmts) or 2 deps (≤8 stmts)
            let ndeps = module.dependencies.len();
            if ndeps == 0 || ndeps > 2 { continue; }
            if ndeps == 1 && stmts.len() > 15 { continue; }
            if ndeps == 2 && stmts.len() > 8 { continue; }
            let dep_id = module.dependencies[0];
            if let Some(dep_mod) = registry.modules.get(&dep_id) {
                if let Some(dep_name) = &dep_mod.name {
                    if is_meaningful_name(dep_name) {
                        new_dep_names.push((module.module_id, dep_name.clone()));
                    }
                }
            }
        }
        if new_dep_names.is_empty() { break; }
        _dep_named += new_dep_names.len();
        for (mod_id, name) in new_dep_names {
            if let Some(module) = registry.modules.get_mut(&mod_id) {
                if module.name.is_none() {
                    module.name = Some(name);
                }
            }
        }
    }

    // Sync names from modules to factories (factories is a copy made at detection time)
    for (func_id, factory) in registry.factories.iter_mut() {
        if factory.name.is_none() {
            if let Some(mod_id) = registry.function_to_module.get(func_id) {
                if let Some(module) = registry.modules.get(mod_id) {
                    if module.name.is_some() {
                        factory.name = module.name.clone();
                    }
                }
            }
        }
    }

    // Count how many modules have names now
    let named_total = registry.modules.values().filter(|m| m.name.is_some()).count();
    let total = registry.modules.len();
    eprintln!("[pipeline] module naming: {named_total}/{total} named ({inferred_count} inferred, {reexport_count} re-export)");

    // PHASE 1: Detect closure_N = require(id) and propagate module names to closure slots
    propagate_module_names_to_closures(functions, registry, closure_ctx);

    // Capture effective names for lookup
    let effective_names: BTreeMap<u32, String> = registry
        .modules
        .iter()
        .map(|(id, m)| {
            let name = m
                .name
                .clone()
                .unwrap_or_else(|| format!("module_{id}"));

            // Ensure valid identifier
            let sanitized = if name.chars().all(|c| c.is_ascii_digit()) {
                format!("v{name}")
            } else {
                name
            };
            (*id, sanitized)
        })
        .collect();

    // Precompute: for each function, find its ancestor factory function ID
    // This allows nested functions to resolve dependencyMap via their parent factory
    let func_to_factory: BTreeMap<u32, u32> = if let Some(ctx) = closure_ctx {
        let mut map = BTreeMap::new();
        for &fid in functions.keys() {
            if registry.function_to_module.contains_key(&fid) {
                map.insert(fid, fid);
            } else {
                // Walk up parent chain to find a factory
                let mut current = fid;
                for _ in 0..MAX_PARENT_CHAIN_DEPTH {
                    if let Some(&parent) = ctx.parent_function.get(&current) {
                        if registry.function_to_module.contains_key(&parent) {
                            map.insert(fid, parent);
                            break;
                        }
                        current = parent;
                    } else {
                        break;
                    }
                }
            }
        }
        map
    } else {
        BTreeMap::new()
    };

    // Iterate all functions to find `require` calls
    let mut all_func_ids: Vec<_> = functions.keys().copied().collect();
    all_func_ids.sort();
    for func_id in &all_func_ids {
        let stmts = functions.get_mut(func_id).unwrap();
        // Determine the effective factory function ID for require resolution
        let effective_func_id = func_to_factory.get(func_id).copied().unwrap_or(*func_id);

        let mut renames = BTreeMap::<String, String>::new();

        // Track register/variable sources for simple constant propagation
        // Map Name -> Parameter Index
        let mut reg_params: HashMap<String, u32> = HashMap::new();
        // Map Name -> (Base Name, Index) for array/property loads
        let mut reg_props: HashMap<String, (String, u32)> = HashMap::new();

        for stmt in stmts.iter() {
            // 1. Analyze assignments to track data flow
            if let Statement::Assign { target, value } = stmt {
                let var_name = target_to_key(target);

                if let Some(name) = var_name {
                    // Try to infer parameter index from value
                    let param_idx = match value {
                        Expression::Value(Value::Parameter(idx)) => Some(*idx),
                        Expression::Value(Value::Variable(v)) => {
                            FactoryRoles::extract_param_index(v)
                        }
                        _ => None,
                    };

                    if let Some(idx) = param_idx {
                        reg_params.insert(name.clone(), idx);
                    }

                    match value {
                        Expression::Member {
                            object,
                            property: crate::ir::PropertyKey::Index(idx),
                            ..
                        } => {
                            let base_name = match &**object {
                                Expression::Value(Value::Register(r)) => Some(format!("r{r}")),
                                Expression::Value(Value::Variable(n)) => Some(n.clone()),
                                Expression::Value(Value::Parameter(i)) => Some(format!("arg{i}")),
                                _ => None,
                            };
                            if let Some(base) = base_name {
                                reg_props.insert(name.clone(), (base, *idx as u32));
                            }
                        }
                        Expression::Value(Value::Register(r)) => {
                            let r_name = format!("r{r}");
                            if let Some(prop) = reg_props.get(&r_name) {
                                let val = prop.clone();
                                reg_props.insert(name.clone(), val);
                            }
                            if let Some(param) = reg_params.get(&r_name) {
                                reg_params.insert(name.clone(), *param);
                            }
                        }
                        Expression::Value(Value::Variable(v)) => {
                            if let Some(prop) = reg_props.get(v) {
                                let val = prop.clone();
                                reg_props.insert(name.clone(), val);
                            }
                            if let Some(param) = reg_params.get(v) {
                                reg_params.insert(name.clone(), *param);
                            }
                        }
                        _ => {}
                    }
                }
            }

            // 2. Check for require calls or propagation
            if let Statement::Assign { target, value } = stmt {
                // Case 1: Direct require() call
                if let Some(mod_id) =
                    resolve_require_module(value, effective_func_id, registry, &reg_params, &reg_props)
                {
                    if let Some(name) = effective_names.get(&mod_id) {
                        // Found a target to rename!
                        if let Some(var_name) = target_to_key(target) {
                            renames.insert(var_name, name.clone());
                        }

                        // Handle closure variables specifically
                        if let crate::ir::AssignTarget::ClosureVar { slot, level, .. } = target {
                            if let Some(ctx) = closure_ctx {
                                // Walk up levels to find the defining function
                                let mut defining_func = *func_id;
                                for _ in 0..*level {
                                    if let Some(&p) = ctx.parent_function.get(&defining_func) {
                                        defining_func = p;
                                    }
                                }
                                // Update the slot name
                                ctx.update_slot_variable(defining_func, *slot, name.clone());
                            }
                        }
                    }
                } else if matches!(
                    value,
                    Expression::Value(Value::Variable(_)) | Expression::Value(Value::Register(_))
                ) {
                    // Case 2: Simple assignment (x = y)
                    let source_name = match value {
                        Expression::Value(Value::Variable(v)) => Some(v.clone()),
                        Expression::Value(Value::Register(r)) => Some(format!("r{r}")),
                        _ => None,
                    };

                    if let Some(src) = source_name {
                        if let Some(name) = renames.get(&src).cloned() {
                            if let Some(var_name) = target_to_key(target) {
                                renames.insert(var_name, name.clone());
                            }
                            if let crate::ir::AssignTarget::ClosureVar { slot, level, .. } = target
                            {
                                if let Some(ctx) = closure_ctx {
                                    let mut defining_func = *func_id;
                                    for _ in 0..*level {
                                        if let Some(&p) = ctx.parent_function.get(&defining_func) {
                                            defining_func = p;
                                        }
                                    }
                                    ctx.update_slot_variable(defining_func, *slot, name.clone());
                                }
                            }
                        }
                    }
                } else if let Expression::Call {
                    callee: _,
                    arguments,
                } = value
                {
                    // Case 3: Wrapper call (x = _interopDefault(y))
                    let arg = if arguments.len() == 1 {
                        Some(&arguments[0])
                    } else if arguments.len() == 2 {
                        Some(&arguments[1])
                    } else {
                        None
                    };

                    if let Some(arg_expr) = arg {
                        // Sub-case 3.1: Argument is a nested require() call?
                        let mut propagated_name = None;

                        if let Some(mod_id) = resolve_require_module(
                            arg_expr,
                            effective_func_id,
                            registry,
                            &reg_params,
                            &reg_props,
                        ) {
                            if let Some(name) = effective_names.get(&mod_id) {
                                propagated_name = Some(name.clone());
                            }
                        }

                        // Sub-case 3.2: Argument is a variable holding a module?
                        if propagated_name.is_none() {
                            if let Expression::Value(val) = arg_expr {
                                let arg_name = match val {
                                    Value::Variable(v) => Some(v.clone()),
                                    Value::Register(r) => Some(format!("r{r}")),
                                    _ => None,
                                };
                                if let Some(arg_v) = arg_name {
                                    if let Some(name) = renames.get(&arg_v).cloned() {
                                        propagated_name = Some(name);
                                    }
                                }
                            }
                        }

                        if let Some(name) = propagated_name {
                            if let Some(var_name) = target_to_key(target) {
                                renames.insert(var_name, name.clone());
                            }
                            // 4. Update Closure Context if it's a closure variable
                            if let crate::ir::AssignTarget::ClosureVar { slot, level, .. } = target
                            {
                                if let Some(ctx) = closure_ctx {
                                    let mut defining_func = *func_id;
                                    for _ in 0..*level {
                                        if let Some(&p) = ctx.parent_function.get(&defining_func) {
                                            defining_func = p;
                                        }
                                    }
                                    ctx.update_slot_variable(defining_func, *slot, name.clone());
                                }
                            } else if let crate::ir::AssignTarget::Variable(v) = target {
                                // Fallback: handle "closure_N" as a slot update
                                if let Some(slot_id_str) = v.strip_prefix("closure_") {
                                    if let Ok(slot) = slot_id_str.parse::<u32>() {
                                        if let Some(ctx) = closure_ctx {
                                            ctx.update_slot_variable(*func_id, slot, name.clone());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Apply renames to current function statements (rename usages)
        if !renames.is_empty() {
            crate::analysis::naming::rename_variables_in_stmts(stmts, &renames);
        }
    }
}
