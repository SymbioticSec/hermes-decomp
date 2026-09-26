// Maximum iterations for re-export name propagation.
// Convergence typically occurs in 2-3 iterations; 20 guarantees termination.
const MAX_REEXPORT_ITERATIONS: usize = 20;

use super::is_meaningful_require_name;
use super::require_resolution::{extract_require_module_id, resolve_require_module};
use crate::analysis::metro::detection::is_meaningful_name;
use crate::analysis::metro::registry::{FactoryRoles, MetroRegistry};
use crate::analysis::ClosureContext;
use crate::ir::{target_to_key, Binding, Expression, PropertyKey, Statement, Value};
use std::collections::BTreeMap;
use std::collections::HashMap;

// Scan all factory bodies for `varName = require(depId)` patterns.
// When varName is a meaningful name and the dep module has no name yet,
// use varName to name that module. Uses voting: the most frequent name wins.
pub(super) fn reverse_require_naming(
    functions: &BTreeMap<u32, Vec<Statement>>,
    registry: &mut MetroRegistry,
) {
    // Collect votes: dep_module_id → HashMap<name, count>
    let mut votes: BTreeMap<u32, HashMap<String, u32>> = BTreeMap::new();

    let mut rr_mod_ids: Vec<_> = registry.modules.keys().copied().collect();
    rr_mod_ids.sort();
    for mid in &rr_mod_ids {
        let module = &registry.modules[mid];
        let fid = module.function_id;
        let Some(stmts) = functions.get(&fid) else {
            continue;
        };

        // Build the same tracking context as the main propagation loop
        let mut reg_params: HashMap<String, u32> = HashMap::new();
        let mut reg_props: HashMap<String, (String, u32)> = HashMap::new();

        for stmt in stmts {
            let (var_name, value) = match stmt {
                Statement::Assign { target, value } => (target_to_key(target), value),
                Statement::Let { name, value, .. } => (Some(name.clone()), value),
                _ => continue,
            };
            {
                if let Some(ref name) = var_name {
                    let param_idx = match value {
                        Expression::Value(Value::Parameter(idx)) => Some(*idx),
                        Expression::Value(Value::Binding(Binding::Variable(v))) => {
                            FactoryRoles::extract_param_index(v)
                        }
                        _ => None,
                    };
                    if let Some(idx) = param_idx {
                        reg_params.insert(name.clone(), idx);
                    }

                    // Track array/property loads (for dependency array access)
                    match value {
                        Expression::Member {
                            object,
                            property: PropertyKey::Index(idx),
                            ..
                        } => {
                            let base_name = match &**object {
                                Expression::Value(Value::Binding(Binding::Register(r))) => {
                                    Some(format!("r{r}"))
                                }
                                Expression::Value(Value::Binding(Binding::Variable(n))) => {
                                    Some(n.clone())
                                }
                                Expression::Value(Value::Parameter(i)) => Some(format!("arg{i}")),
                                _ => None,
                            };
                            if let Some(base) = base_name {
                                reg_props.insert(name.clone(), (base, *idx as u32));
                            }
                        }
                        Expression::Value(Value::Binding(Binding::Register(r))) => {
                            let r_name = format!("r{r}");
                            if let Some(prop) = reg_props.get(&r_name) {
                                reg_props.insert(name.clone(), prop.clone());
                            }
                            if let Some(param) = reg_params.get(&r_name) {
                                reg_params.insert(name.clone(), *param);
                            }
                        }
                        Expression::Value(Value::Binding(Binding::Variable(v))) => {
                            if let Some(prop) = reg_props.get(v) {
                                reg_props.insert(name.clone(), prop.clone());
                            }
                            if let Some(param) = reg_params.get(v) {
                                reg_params.insert(name.clone(), *param);
                            }
                        }
                        _ => {}
                    }
                }

                // Now try to resolve value as require() call
                let mod_id = resolve_require_module(value, fid, registry, &reg_params, &reg_props)
                    .or_else(|| {
                        // Also handle wrapper calls: _interopDefault(require(N))
                        if let Expression::Call { arguments, .. } = value {
                            for arg in arguments {
                                if let Some(id) = resolve_require_module(
                                    arg,
                                    fid,
                                    registry,
                                    &reg_params,
                                    &reg_props,
                                ) {
                                    return Some(id);
                                }
                            }
                        }
                        None
                    });

                if let Some(dep_id) = mod_id {
                    if let Some(ref name) = var_name {
                        if is_meaningful_require_name(name) {
                            log::trace!(
                                target: "require",
                                "reverse-name: module {dep_id} <- var {name:?} (from factory fn {fid})"
                            );
                            *votes
                                .entry(dep_id)
                                .or_default()
                                .entry(name.clone())
                                .or_insert(0) += 1;
                        } else {
                            // The capturing variable is generic (`_require`, `tmp`, `rN`, ...),
                            // so it cannot name the dependency. This is the common reason a
                            // required module stays unnamed even though it is clearly used.
                            log::trace!(
                                target: "require",
                                "reverse-name: module {dep_id} required as {name:?} in fn {fid} but name is generic, no vote"
                            );
                        }
                    }
                }
            }
        }
    }

    // Apply winning names to unnamed modules
    let mut named_count = 0;
    let mut vote_keys: Vec<_> = votes.keys().copied().collect();
    vote_keys.sort();
    for dep_id in &vote_keys {
        let name_votes = &votes[dep_id];
        let Some(module) = registry.modules.get_mut(dep_id) else {
            continue;
        };
        if module.name.is_some() {
            continue;
        }

        // Pick the most popular name, filtering out meaningless names
        if let Some((best_name, _count)) = name_votes
            .iter()
            .filter(|(name, _)| is_meaningful_name(name))
            .max_by(|(n1, c1), (n2, c2)| c1.cmp(c2).then_with(|| n2.cmp(n1)))
        {
            module.name = Some(best_name.clone());
            named_count += 1;
        }
    }

    if named_count > 0 {
        log::debug!("[pipeline] reverse require naming: {named_count} modules named");
    }
}

// Propagate names from named modules to unnamed re-export wrappers.
pub(super) fn propagate_reexport_names(
    functions: &BTreeMap<u32, Vec<Statement>>,
    registry: &mut MetroRegistry,
) -> usize {
    let mut total_named = 0;
    for _ in 0..MAX_REEXPORT_ITERATIONS {
        let mut new_names: Vec<(u32, String)> = Vec::new();

        let mut re_mod_ids: Vec<_> = registry.modules.keys().copied().collect();
        re_mod_ids.sort();
        for mid in &re_mod_ids {
            let module = &registry.modules[mid];
            if module.name.is_some() {
                continue;
            }
            let Some(stmts) = functions.get(&module.function_id) else {
                continue;
            };
            if stmts.len() > 100 {
                continue;
            }

            let mut reg_params: HashMap<String, u32> = HashMap::new();
            let mut reg_props: HashMap<String, (String, u32)> = HashMap::new();

            for stmt in stmts {
                let (opt_name, value) = match stmt {
                    Statement::Assign { target, value } => (target_to_key(target), value),
                    Statement::Let { name, value, .. } => (Some(name.clone()), value),
                    _ => continue,
                };
                if let Some(name) = opt_name {
                    let param_idx = match value {
                        Expression::Value(Value::Parameter(idx)) => Some(*idx),
                        Expression::Value(Value::Binding(Binding::Variable(v))) => {
                            FactoryRoles::extract_param_index(v)
                        }
                        _ => None,
                    };
                    if let Some(idx) = param_idx {
                        reg_params.insert(name.clone(), idx);
                    }
                    if let Expression::Member {
                        object,
                        property: PropertyKey::Index(idx),
                        ..
                    } = value
                    {
                        let base_name = match &**object {
                            Expression::Value(Value::Binding(Binding::Register(r))) => {
                                Some(format!("r{r}"))
                            }
                            Expression::Value(Value::Binding(Binding::Variable(n))) => {
                                Some(n.clone())
                            }
                            Expression::Value(Value::Parameter(i)) => Some(format!("arg{i}")),
                            _ => None,
                        };
                        if let Some(base) = base_name {
                            reg_props.insert(name.clone(), (base, *idx as u32));
                        }
                    }
                }
            }

            // Look for export assignments that require a dependency
            for stmt in stmts {
                if let Statement::Assign { target, value } = stmt {
                    let is_export = match target {
                        crate::ir::AssignTarget::Binding(Binding::Variable(n)) => {
                            FactoryRoles::matches_exports_name(n)
                        }
                        crate::ir::AssignTarget::Member { object, .. } => match object {
                            Expression::Value(Value::Binding(Binding::Variable(n))) => {
                                FactoryRoles::matches_module_name(n)
                                    || FactoryRoles::matches_exports_name(n)
                            }
                            Expression::Value(Value::Parameter(idx))
                                if FactoryRoles::is_module_idx(*idx)
                                    || FactoryRoles::is_exports_idx(*idx) =>
                            {
                                true
                            }
                            _ => false,
                        },
                        _ => false,
                    };

                    if !is_export {
                        continue;
                    }

                    let dep_id = resolve_require_module(
                        value,
                        module.function_id,
                        registry,
                        &reg_params,
                        &reg_props,
                    )
                    .or_else(|| {
                        if let Expression::Member { object, .. } = value {
                            resolve_require_module(
                                object,
                                module.function_id,
                                registry,
                                &reg_params,
                                &reg_props,
                            )
                        } else {
                            None
                        }
                    })
                    .or_else(|| {
                        if let Expression::Call { arguments, .. } = value {
                            for arg in arguments {
                                if let Some(id) = resolve_require_module(
                                    arg,
                                    module.function_id,
                                    registry,
                                    &reg_params,
                                    &reg_props,
                                ) {
                                    return Some(id);
                                }
                                if let Expression::Member { object, .. } = arg {
                                    if let Some(id) = resolve_require_module(
                                        object,
                                        module.function_id,
                                        registry,
                                        &reg_params,
                                        &reg_props,
                                    ) {
                                        return Some(id);
                                    }
                                }
                            }
                            None
                        } else {
                            None
                        }
                    });

                    if let Some(dep_id) = dep_id {
                        if let Some(dep_module) = registry.modules.get(&dep_id) {
                            if let Some(dep_name) = &dep_module.name {
                                new_names.push((module.module_id, dep_name.clone()));
                                break;
                            }
                        }
                    }
                }
            }

            // Check for __exportStar(require(dep), exports) patterns
            if new_names.iter().all(|(id, _)| *id != module.module_id) {
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
                            let dep_id = resolve_require_module(
                                &arguments[0],
                                module.function_id,
                                registry,
                                &reg_params,
                                &reg_props,
                            )
                            .or_else(|| {
                                if let Expression::Value(Value::Binding(Binding::Variable(v))) =
                                    &arguments[0]
                                {
                                    for s in stmts {
                                        if let Statement::Assign { target, value } = s {
                                            if let Some(tgt_name) = target_to_key(target) {
                                                if &tgt_name == v {
                                                    return resolve_require_module(
                                                        value,
                                                        module.function_id,
                                                        registry,
                                                        &reg_params,
                                                        &reg_props,
                                                    );
                                                }
                                            }
                                        }
                                        if let Statement::Let { name, value, .. } = s {
                                            if name == v {
                                                return resolve_require_module(
                                                    value,
                                                    module.function_id,
                                                    registry,
                                                    &reg_params,
                                                    &reg_props,
                                                );
                                            }
                                        }
                                    }
                                }
                                None
                            });
                            if let Some(dep_id) = dep_id {
                                if let Some(dep_module) = registry.modules.get(&dep_id) {
                                    if let Some(dep_name) = &dep_module.name {
                                        new_names.push((module.module_id, dep_name.clone()));
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if new_names.is_empty() {
            break;
        }
        let count = new_names.len();
        for (mod_id, name) in new_names {
            if let Some(module) = registry.modules.get_mut(&mod_id) {
                if module.name.is_none() {
                    module.name = Some(name);
                }
            }
        }
        total_named += count;
    }
    total_named
}

// PHASE 1: Detect closure_N = require(id) and propagate module names to closure slots.
pub(crate) fn propagate_module_names_to_closures(
    functions: &mut BTreeMap<u32, Vec<Statement>>,
    registry: &MetroRegistry,
    closure_ctx: &mut Option<ClosureContext>,
) {
    // A slot holding the string `"GuildSelector"`, or the import of a module
    // named GuildSelector, next to a function named GuildSelector must not
    // take that name: the two would bind the same identifier at module level.
    // Only the functions declared in the slot owner's own body count; the same
    // name declared in another module is no conflict, and the import must keep
    // its name there.
    let mut declared_names: BTreeMap<u32, std::collections::HashSet<String>> = BTreeMap::new();
    let module_names: BTreeMap<u32, String> = registry
        .modules
        .iter()
        .map(|(id, module)| {
            let name = module
                .name
                .clone()
                .unwrap_or_else(|| format!("module_{id}"));
            (*id, name)
        })
        .collect();

    let mut closure_func_ids: Vec<_> = functions.keys().copied().collect();
    closure_func_ids.sort();
    for func_id in &closure_func_ids {
        let stmts = &functions[func_id];
        let mut reg_params: HashMap<String, u32> = HashMap::new();
        let mut reg_props: HashMap<String, (String, u32)> = HashMap::new();
        // A slot written more than once holds different values over time (an
        // import first, a class later), and a name taken from one of those
        // stores would label the other. Only a slot with a single store may be
        // named after what is stored in it.
        let mut slot_writes: HashMap<(u32, u32), usize> = HashMap::new();
        for stmt in stmts.iter() {
            if let Statement::Assign { target, .. } = stmt {
                if let Some(key) = closure_write_target(target) {
                    *slot_writes.entry(key).or_insert(0) += 1;
                }
            }
        }

        for stmt in stmts.iter() {
            // Track data flow from both Assign and Let for require resolution
            {
                let (opt_name, value) = match stmt {
                    Statement::Assign { target, value } => (target_to_key(target), value),
                    Statement::Let { name, value, .. } => (Some(name.clone()), value),
                    _ => (
                        None,
                        &Expression::Value(Value::Constant(crate::ir::Constant::Undefined)),
                    ),
                };
                if let Some(name) = opt_name {
                    let param_idx = match value {
                        Expression::Value(Value::Parameter(idx)) => Some(*idx),
                        Expression::Value(Value::Binding(Binding::Variable(v))) => {
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
                            property: PropertyKey::Index(idx),
                            ..
                        } => {
                            let base_name = match &**object {
                                Expression::Value(Value::Binding(Binding::Register(r))) => {
                                    Some(format!("r{r}"))
                                }
                                Expression::Value(Value::Binding(Binding::Variable(n))) => {
                                    Some(n.clone())
                                }
                                Expression::Value(Value::Parameter(i)) => Some(format!("arg{i}")),
                                _ => None,
                            };
                            if let Some(base) = base_name {
                                reg_props.insert(name.clone(), (base, *idx as u32));
                            }
                        }
                        Expression::Value(Value::Binding(Binding::Register(r))) => {
                            let r_name = format!("r{r}");
                            if let Some(prop) = reg_props.get(&r_name) {
                                reg_props.insert(name.clone(), prop.clone());
                            }
                            if let Some(param) = reg_params.get(&r_name) {
                                reg_params.insert(name.clone(), *param);
                            }
                        }
                        Expression::Value(Value::Binding(Binding::Variable(v))) => {
                            if let Some(prop) = reg_props.get(v) {
                                reg_props.insert(name.clone(), prop.clone());
                            }
                            if let Some(param) = reg_params.get(v) {
                                reg_params.insert(name.clone(), *param);
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Check if Assign target is a closure variable
            if let Statement::Assign { target, value } = stmt {
                let Some((level, slot)) = closure_write_target(target) else {
                    continue;
                };
                let owner = closure_slot_owner(*func_id, level, closure_ctx);
                let module_id =
                    resolve_require_module(value, *func_id, registry, &reg_params, &reg_props)
                        .or_else(|| {
                            if let Expression::Call { arguments, .. } = value {
                                for arg in arguments {
                                    if let Some(id) = resolve_require_module(
                                        arg,
                                        *func_id,
                                        registry,
                                        &reg_params,
                                        &reg_props,
                                    ) {
                                        return Some(id);
                                    }
                                }
                            }
                            None
                        })
                        .or_else(|| extract_require_module_id(value));

                let name = if let Some(module_id) = module_id {
                    module_names.get(&module_id).cloned()
                } else if slot_writes.get(&(level, slot)).copied().unwrap_or(0) == 1 {
                    name_stored_in_slot(value)
                } else {
                    None
                };
                let Some(name) = name else {
                    continue;
                };
                if let Some(ctx) = closure_ctx {
                    let taken = declared_names
                        .entry(owner)
                        .or_insert_with(|| ctx.declared_function_names(owner));
                    if taken.contains(&name) {
                        continue;
                    }
                    ctx.update_slot_variable(owner, slot, name);
                }
            }
        }
    }
}

// `(level, slot)` of a write into an env slot. `closure_5` is level 0.
// `closure_2_14` is the parent env two hops up, slot 14. A raw `ClosureVar`
// carries the level on the binding.
fn closure_write_target(target: &crate::ir::AssignTarget) -> Option<(u32, u32)> {
    match target {
        crate::ir::AssignTarget::Binding(Binding::Variable(name)) => parse_closure_binding(name),
        crate::ir::AssignTarget::Binding(Binding::ClosureVar { slot, level }) => {
            Some((*level, *slot))
        }
        _ => None,
    }
}

fn parse_closure_binding(name: &str) -> Option<(u32, u32)> {
    let rest = name.strip_prefix("closure_")?;
    let (head, tail) = rest.split_once('_').unwrap_or((rest, ""));
    let a: u32 = head.parse().ok()?;
    if tail.is_empty() {
        return Some((0, a));
    }
    let b: u32 = tail.parse().ok()?;
    if tail.contains('_') {
        return None;
    }
    Some((a, b))
}

fn closure_slot_owner(
    func_id: u32,
    level: u32,
    closure_ctx: &mut Option<crate::analysis::ClosureContext>,
) -> u32 {
    if level == 0 {
        return func_id;
    }
    // A slot at another level belongs to another scope; naming this
    // function's own slot instead put a class name on the `global` parameter.
    closure_ctx
        .as_ref()
        .and_then(|ctx| ctx.slot_owner(func_id, level))
        .unwrap_or(u32::MAX)
}

// The name a slot may take from the value stored into it. An import binding
// or a constant identifier already in scope. A property read contributes its
// key only when that key is the value (`constants.EVENT` is the event).
// `length` is not a name: almost every array parameter has one.
fn name_stored_in_slot(value: &Expression) -> Option<String> {
    match value {
        Expression::Value(Value::Binding(Binding::Variable(name))) if stored_name_ok(name) => {
            Some(name.clone())
        }
        Expression::Value(Value::Constant(crate::ir::Constant::String(s))) if stored_name_ok(s) => {
            Some(s.clone())
        }
        Expression::Member {
            object, property, ..
        } => {
            let crate::ir::PropertyKey::Ident(prop) = property else {
                return None;
            };
            let Expression::Value(Value::Binding(Binding::Variable(base))) = object.as_ref() else {
                return None;
            };
            // The base has to already be a real name (an import, a constant).
            // The property is the value being stored, except `default`, which
            // is the module itself, and `length`, which is not a name.
            if !stored_name_ok(base) {
                return None;
            }
            if prop == "default" {
                Some(base.clone())
            } else if stored_name_ok(prop) {
                Some(prop.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn stored_name_ok(name: &str) -> bool {
    if name == "length" || name == "constructor" || name == "prototype" || name == "__proto__" {
        return false;
    }
    crate::analysis::metro::is_usable_module_specifier(name)
        && !crate::analysis::metro::is_generic_module_specifier(name)
        && crate::util::is_valid_identifier(name)
}
