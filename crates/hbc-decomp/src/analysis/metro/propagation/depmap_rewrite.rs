// Rewrite `dependencyMap[N]` / aliases into absolute module IDs.
//
// Metro factories receive a dependency array as their last parameter. The
// compiler lowers `require('./foo')` to `require(dependencyMap[k])` where `k`
// indexes that factory's dependency list.
//
// This pass MUST run AFTER `resolve_closures`: nested functions capture the
// map as a ClosureVar, which only becomes `Variable("dependencyMap")` (or an
// alias) once slots are resolved. Running earlier leaves ~half the references
// unresolved (HBC96 reference baseline: 162k → needs late rewrite).

use super::super::registry::{FactoryRoles, MetroRegistry};
use super::is_dep_array_name;
use crate::analysis::ClosureContext;
use crate::ir::{AssignTarget, Binding, Expression, Statement, Value};
use std::collections::{BTreeMap, HashSet};

mod require_calls;
mod walk;

pub fn rewrite_dependency_map_indices(
    functions: &mut BTreeMap<u32, Vec<Statement>>,
    registry: &MetroRegistry,
    closure_ctx: &Option<ClosureContext>,
    param_names: &BTreeMap<u32, Vec<Option<String>>>,
) {
    // factory function_id → dependency list and the factory's real param layout
    let mut factory_deps: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let mut factory_roles: BTreeMap<u32, FactoryRoles> = BTreeMap::new();
    for module in registry.modules.values() {
        factory_roles.insert(module.function_id, module.roles.clone());
        if !module.dependencies.is_empty() {
            factory_deps.insert(module.function_id, module.dependencies.clone());
        }
    }
    for (func_id, mod_id) in &registry.function_to_module {
        if !factory_deps.contains_key(func_id) {
            if let Some(module) = registry.modules.get(mod_id) {
                if !module.dependencies.is_empty() {
                    factory_deps.insert(*func_id, module.dependencies.clone());
                }
            }
        }
    }
    if factory_deps.is_empty() {
        return;
    }

    // Resolve deps for every function via parent_function chain.
    let mut func_deps: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let ids: Vec<u32> = functions.keys().copied().collect();
    for fid in &ids {
        if let Some(deps) = factory_deps.get(fid) {
            func_deps.insert(*fid, deps.clone());
            continue;
        }
        if let Some(ctx) = closure_ctx {
            let mut current = *fid;
            let mut seen = HashSet::new();
            seen.insert(current);
            while let Some(&parent) = ctx.parent_function.get(&current) {
                if !seen.insert(parent) {
                    break;
                }
                if let Some(deps) = factory_deps.get(&parent) {
                    func_deps.insert(*fid, deps.clone());
                    break;
                }
                current = parent;
            }
        }
    }

    let mut rewrites = 0u64;
    for fid in ids {
        let Some(deps) = func_deps.get(&fid).cloned() else {
            continue;
        };
        let roles = factory_roles
            .get(&fid)
            .cloned()
            .or_else(|| {
                closure_ctx.as_ref().and_then(|ctx| {
                    let mut current = fid;
                    let mut seen = HashSet::new();
                    seen.insert(current);
                    while let Some(&parent) = ctx.parent_function.get(&current) {
                        if !seen.insert(parent) {
                            break;
                        }
                        if let Some(r) = factory_roles.get(&parent) {
                            return Some(r.clone());
                        }
                        current = parent;
                    }
                    None
                })
            })
            .unwrap_or_else(FactoryRoles::standard);
        if let Some(stmts) = functions.get_mut(&fid) {
            // Collect local aliases of the dependency array (e.g.
            // `const map = dependencyMap`, `let d = arg4`).
            // `loose` keeps names that were dropped because the slot is reused
            // later in the function. A require() argument may still be a real
            // index even when the same name is overwritten afterwards.
            let (mut aliases, loose) = collect_depmap_aliases(stmts, &roles);
            let require_names = require_calls::require_callee_names(
                fid,
                &roles,
                param_names,
                closure_ctx,
                registry,
            );
            let before = rewrites;
            rewrites += walk::rewrite_stmts(stmts, &deps, &mut aliases, &roles);
            rewrites += require_calls::rewrite_require_dep_calls(
                stmts,
                &deps,
                &loose,
                &require_names,
                &roles,
            );
            if rewrites > before {
                log::trace!(target: "depmap", "fn{fid}: {} rewrites", rewrites - before);
            }
        }
    }
    if rewrites > 0 {
        log::debug!(
            "[metro] late-rewrote {rewrites} dependencyMap[N] indices to absolute module IDs"
        );
    }
}

// Names known to refer to the factory dependency array.
fn collect_depmap_aliases(
    stmts: &[Statement],
    roles: &FactoryRoles,
) -> (HashSet<String>, HashSet<String>) {
    let mut aliases: HashSet<String> = HashSet::new();
    // Always recognize the canonical names (including names we rewrite
    // factory-captured slots to via get_slot_name).
    aliases.insert("dependencyMap".into());
    aliases.insert("deps".into());
    aliases.insert("_dependencyMap".into());
    // After closure naming, heavily-indexed captures may still be called
    // `dependencyMap` or a unique `dependencyMap2`, handled below by prefix.

    fn walk(stmts: &[Statement], aliases: &mut HashSet<String>, roles: &FactoryRoles) {
        for stmt in stmts {
            match stmt {
                Statement::Let { name, value, .. }
                | Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable(name)),
                    value,
                } => {
                    if expr_is_depmap_root(value, aliases, roles) {
                        aliases.insert(name.clone());
                    }
                }
                Statement::Assign {
                    target: AssignTarget::Binding(Binding::Register(r)),
                    value,
                } => {
                    if expr_is_depmap_root(value, aliases, roles) {
                        aliases.insert(format!("r{r}"));
                    }
                }
                _ => {}
            }
            // Nested bodies: re-scan with same alias set (conservative, aliases
            // from outer scopes are valid inside).
            let mut nested: Vec<&[Statement]> = Vec::new();
            match stmt {
                Statement::If {
                    then_body,
                    else_body,
                    ..
                } => {
                    nested.push(then_body);
                    nested.push(else_body);
                }
                Statement::While { body, .. }
                | Statement::DoWhile { body, .. }
                | Statement::For { body, .. }
                | Statement::ForIn { body, .. }
                | Statement::ForOf { body, .. }
                | Statement::Block(body) => nested.push(body),
                Statement::TryCatch {
                    try_body,
                    catch_body,
                    finally_body,
                    ..
                } => {
                    nested.push(try_body);
                    nested.push(catch_body);
                    nested.push(finally_body);
                }
                Statement::Switch { cases, default, .. } => {
                    for (_, b) in cases {
                        nested.push(b);
                    }
                    if let Some(d) = default {
                        nested.push(d);
                    }
                }
                _ => {}
            }
            for n in nested {
                walk(n, aliases, roles);
            }
        }
    }
    walk(stmts, &mut aliases, roles);

    // A name is only a dependency array if it always holds one. HBC >=97 reuses
    // a slot for unrelated values, and the factory role naming still calls that
    // slot `dependencyMap`, so a name alone proves nothing: a slot holding the
    // result of `jwt.split(".")` was read as the dependency array, and
    // `atob(jwt.split(".")[1])` was rewritten into `require(<dependency 1>)`,
    // which is a call the program never makes. Any name that is also assigned
    // something that is not the dependency array is dropped here.
    let loose = aliases.clone();
    let mut reused: HashSet<String> = HashSet::new();
    collect_reused(stmts, &aliases, roles, &mut reused);
    aliases.retain(|name| !reused.contains(name));
    (aliases, loose)
}

// Names that denote the factory's require parameter, including a later rename
// (`require` written down as `Logger` by a naming pass). A call through one of
// these is `require(dependencyMap[k])` even when the dependency-array name was
// reused elsewhere and dropped from the strict alias set.
fn collect_reused(
    stmts: &[Statement],
    aliases: &HashSet<String>,
    roles: &FactoryRoles,
    reused: &mut HashSet<String>,
) {
    use crate::ir::Visitor;
    struct V<'a> {
        aliases: &'a HashSet<String>,
        roles: &'a FactoryRoles,
        reused: &'a mut HashSet<String>,
    }
    impl<'a, 'b> Visitor<'b> for V<'a> {
        fn visit_statement(&mut self, s: &'b Statement) {
            let bound = match s {
                Statement::Let { name, value, .. }
                | Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable(name)),
                    value,
                } => Some((name, value)),
                _ => None,
            };
            if let Some((name, value)) = bound {
                if self.aliases.contains(name)
                    && !expr_is_depmap_root(value, self.aliases, self.roles)
                {
                    self.reused.insert(name.clone());
                }
            }
            self.walk_statement(s);
        }
    }
    let mut v = V {
        aliases,
        roles,
        reused,
    };
    for s in stmts {
        v.visit_statement(s);
    }
}

pub(super) fn expr_is_depmap_root(
    expr: &Expression,
    aliases: &HashSet<String>,
    roles: &FactoryRoles,
) -> bool {
    match expr {
        Expression::Value(Value::Binding(Binding::Variable(n))) => {
            aliases.contains(n) || is_dep_array_name(n, roles)
        }
        Expression::Value(Value::Parameter(idx)) => roles.deps_idx == Some(*idx),
        _ => false,
    }
}
