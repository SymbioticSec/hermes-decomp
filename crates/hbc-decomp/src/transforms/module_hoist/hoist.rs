// Per-module hoist orchestration: collect, inject, rewrite.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::analysis::MetroRegistry;
use crate::ir::{Constant, Expression, MutVisitor, Statement, Value, VarKind};
use crate::util::sanitize_identifier;

use super::detect::{loader_aliases, loader_call, loader_uses};
use super::kinds::{HoistKey, LoaderKind, RESERVED_BINDINGS};
use super::lazy::{collect_descriptor_getter_ids, is_pure_lazy_getter_body, lazy_getter_ids};
use super::names::{
    allocate_binding_name, binding_name_value, collect_existing_binding_names, is_reserved_binding,
    is_reused_binding,
};

// Entry point: hoist eager module loads for every Metro module factory.
// `param_names` is factory + descendant parameter names (IPA or header), used
// so an injected binding never shadows a child param/local.
pub fn hoist_module_loaders(
    all_ir: &mut BTreeMap<u32, Vec<Statement>>,
    registry: &MetroRegistry,
    child_functions: &BTreeMap<u32, Vec<u32>>,
    param_names: &BTreeMap<u32, Vec<String>>,
) {
    let factory_ids: Vec<u32> = registry.function_to_module.keys().copied().collect();
    for factory_id in factory_ids {
        let deps = registry
            .get_module_for_function(factory_id)
            .map(|m| m.dependencies.clone())
            .unwrap_or_default();
        let members = descendants(factory_id, child_functions, registry);
        hoist_one_module(factory_id, &members, &deps, all_ir, registry, param_names);
    }
}

// Closures of one factory. Other Metro factories are siblings even if a
// parent_function edge exists (never happens in practice; defensive).
fn descendants(
    factory_id: u32,
    child_functions: &BTreeMap<u32, Vec<u32>>,
    registry: &MetroRegistry,
) -> Vec<u32> {
    let mut out = vec![factory_id];
    let mut stack = vec![factory_id];
    let mut seen = HashSet::new();
    seen.insert(factory_id);
    while let Some(id) = stack.pop() {
        if let Some(kids) = child_functions.get(&id) {
            for &k in kids {
                if k != factory_id && registry.function_to_module.contains_key(&k) {
                    continue;
                }
                if seen.insert(k) {
                    out.push(k);
                    stack.push(k);
                }
            }
        }
    }
    out
}

fn hoist_one_module(
    factory_id: u32,
    members: &[u32],
    deps: &[u32],
    all_ir: &mut BTreeMap<u32, Vec<Statement>>,
    registry: &MetroRegistry,
    param_names: &BTreeMap<u32, Vec<String>>,
) {
    // Existing module-scope bindings at factory top: (id, kind) -> name.
    let mut existing: HashMap<HoistKey, String> = HashMap::new();
    if let Some(factory) = all_ir.get(&factory_id) {
        let aliases = loader_aliases(factory);
        for s in factory {
            if let Some((name, value)) = binding_name_value(s) {
                if let Some((id, kind, _)) = loader_call(value, &aliases, deps) {
                    existing
                        .entry((id, kind))
                        .or_insert_with(|| name.to_string());
                }
            }
        }
    }

    // Functions used as `get:` descriptor values — lazy regardless of body shape.
    let mut getter_fns: HashSet<u32> = HashSet::new();
    for &fid in members {
        if let Some(stmts) = all_ir.get(&fid) {
            getter_fns.extend(collect_descriptor_getter_ids(stmts));
        }
    }
    // A Metro factory is never itself a lazy getter.
    getter_fns.remove(&factory_id);

    let mut lazy_ids: HashSet<u32> = HashSet::new();
    let mut eager: HashMap<HoistKey, Expression> = HashMap::new();
    let mut existing_names: HashSet<String> = HashSet::new();
    for n in RESERVED_BINDINGS {
        existing_names.insert((*n).to_string());
    }
    for &fid in members {
        if let Some(ps) = param_names.get(&fid) {
            for p in ps {
                if !p.is_empty() {
                    existing_names.insert(p.clone());
                }
            }
        }
        let Some(stmts) = all_ir.get(&fid) else {
            continue;
        };
        collect_existing_binding_names(stmts, &mut existing_names);
        let aliases = loader_aliases(stmts);
        if getter_fns.contains(&fid) {
            for (id, _, _) in loader_uses(stmts, &aliases, deps) {
                lazy_ids.insert(id);
            }
            continue;
        }
        for id in lazy_getter_ids(stmts, &aliases, deps) {
            lazy_ids.insert(id);
        }
        if is_pure_lazy_getter_body(stmts, &aliases, deps) {
            continue;
        }
        for (id, kind, callee) in loader_uses(stmts, &aliases, deps) {
            eager.entry((id, kind)).or_insert(callee);
        }
    }

    let mut hoist: Vec<(HoistKey, Expression)> = eager
        .into_iter()
        .filter(|((id, _), _)| !lazy_ids.contains(id))
        .collect();
    if hoist.is_empty() {
        return;
    }
    hoist.sort_by_key(|(k, _)| *k);

    let mut bindings: BTreeMap<HoistKey, String> = BTreeMap::new();
    let mut inject: Vec<(HoistKey, Expression)> = Vec::new();
    let mut used: HashSet<String> = existing_names;
    for (key, callee) in &hoist {
        if let Some(name) = existing.get(key) {
            bindings.insert(*key, name.clone());
            continue;
        }
        let (id, kind) = *key;
        let base = registry
            .modules
            .get(&id)
            .and_then(|m| m.name.clone())
            .map(|n| sanitize_identifier(&n))
            .filter(|n| !n.is_empty() && !is_reserved_binding(n));
        // Prefer module name; for non-require loaders suffix to avoid interop collision.
        let preferred = base.map(|b| match kind {
            LoaderKind::Require => b,
            LoaderKind::ImportDefault => {
                if b.ends_with("Default") {
                    b
                } else {
                    format!("{b}Default")
                }
            }
            LoaderKind::ImportAll => {
                if b.ends_with("All") {
                    b
                } else {
                    format!("{b}All")
                }
            }
        });
        let name = allocate_binding_name(preferred, id, kind, &mut used);
        bindings.insert(*key, name);
        inject.push((*key, callee.clone()));
    }

    let hoist_keys: BTreeSet<HoistKey> = hoist.iter().map(|(k, _)| *k).collect();
    for &fid in members {
        if let Some(stmts) = all_ir.get_mut(&fid) {
            let aliases = loader_aliases(stmts);
            if getter_fns.contains(&fid) || is_pure_lazy_getter_body(stmts, &aliases, deps) {
                continue;
            }
            let mut rw = LoaderRewriter {
                bindings: &bindings,
                aliases: &aliases,
                keys: &hoist_keys,
                deps,
            };
            if fid == factory_id && !existing.is_empty() {
                for stmt in stmts.iter_mut() {
                    if is_reused_binding(stmt, &existing, &aliases, deps) {
                        continue;
                    }
                    rw.visit_statement(stmt);
                }
            } else {
                rw.visit_statement_list(stmts);
            }
        }
    }

    if !inject.is_empty() {
        if let Some(factory) = all_ir.get_mut(&factory_id) {
            let mut injected: Vec<Statement> = Vec::new();
            for (key, callee) in &inject {
                let name = bindings.get(key).cloned().unwrap_or_default();
                let id = key.0;
                injected.push(Statement::Let {
                    name,
                    value: Expression::Call {
                        callee: Box::new(callee.clone()),
                        arguments: vec![Expression::Value(Value::Constant(Constant::Integer(
                            id as i32,
                        )))],
                    },
                    kind: VarKind::Const,
                });
            }
            injected.extend(std::mem::take(factory));
            *factory = injected;
        }
    }
}

// Rewrite each eager loader call whose (id, kind) is hoisted into its binding.
struct LoaderRewriter<'a> {
    bindings: &'a BTreeMap<HoistKey, String>,
    aliases: &'a HashMap<String, LoaderKind>,
    keys: &'a BTreeSet<HoistKey>,
    deps: &'a [u32],
}

impl MutVisitor for LoaderRewriter<'_> {
    fn visit_expression(&mut self, e: &mut Expression) {
        if let Some((id, kind, _)) = loader_call(e, self.aliases, self.deps) {
            let key = (id, kind);
            if self.keys.contains(&key) {
                if let Some(name) = self.bindings.get(&key) {
                    *e = Expression::Value(Value::Variable(name.clone()));
                    return;
                }
            }
        }
        self.walk_expression(e);
    }
}
