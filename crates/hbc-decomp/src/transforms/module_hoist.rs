// Hoist eager inline module loads into a single module-level binding.
//
// On a modern Metro bundle the loader function (`require`, `importDefault`,
// `importAll`, or a register that was assigned one of them) is called at every use
// site inside a module's functions, so a single dependency shows up many times as
// `importDefault(530).foo()` / `require(4137).bar()`. This pass gives each eagerly
// used module one binding at the top of the module factory (which the ESM classifier
// then turns into an import) and rewrites every eager occurrence to that binding.
//
// It is deliberately conservative:
//   * A module is identified by its absolute id, resolving both `loader(1234)` and
//     `loader(dependencyMap[i])` to the same module, so the two call forms share one
//     binding instead of producing duplicates.
//   * An existing module-scope binding (`let x = require(N)` at the factory top) is
//     reused rather than minting a second one.
//   * Lazy React Native getters (`get: () => require(N).default`, compiled to a tiny
//     arrow whose whole body is one loader access) are left untouched, so lazy module
//     loading is preserved. A module is hoisted only when it is never used lazily.
//   * The exact loader expression is preserved (an `importDefault` stays an
//     `importDefault`), so interop semantics do not change.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::analysis::MetroRegistry;
use crate::ir::{Constant, Expression, MutVisitor, PropertyKey, Statement, Value, VarKind, Visitor};
use crate::util::sanitize_identifier;

const LOADER_NAMES: &[&str] = &["require", "importDefault", "importAll"];

// Entry point: hoist eager module loads for every Metro module factory.
pub fn hoist_module_loaders(
    all_ir: &mut BTreeMap<u32, Vec<Statement>>,
    registry: &MetroRegistry,
    child_functions: &BTreeMap<u32, Vec<u32>>,
) {
    let factory_ids: Vec<u32> = registry.function_to_module.keys().copied().collect();
    for factory_id in factory_ids {
        let deps = registry
            .get_module_for_function(factory_id)
            .map(|m| m.dependencies.clone())
            .unwrap_or_default();
        let members = descendants(factory_id, child_functions);
        hoist_one_module(factory_id, &members, &deps, all_ir, registry);
    }
}

// All functions belonging to a module: the factory plus every transitive child.
fn descendants(factory_id: u32, child_functions: &BTreeMap<u32, Vec<u32>>) -> Vec<u32> {
    let mut out = vec![factory_id];
    let mut stack = vec![factory_id];
    let mut seen = HashSet::new();
    seen.insert(factory_id);
    while let Some(id) = stack.pop() {
        if let Some(kids) = child_functions.get(&id) {
            for &k in kids {
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
) {
    // Existing module-scope bindings (`let x = require(N)` directly in the factory
    // body): module id -> binding name, so they are reused instead of duplicated.
    let mut existing: HashMap<u32, String> = HashMap::new();
    if let Some(factory) = all_ir.get(&factory_id) {
        let aliases = loader_aliases(factory);
        for s in factory {
            if let Statement::Let { name, value, .. } = s {
                if let Some((id, _)) = loader_call(value, &aliases, deps) {
                    existing.entry(id).or_insert_with(|| name.clone());
                }
            }
        }
    }

    // Classify every module id used across the module as lazy (single-loader getter
    // arrow) or eager, recording the loader callee for the eager ones.
    let mut lazy_ids: HashSet<u32> = HashSet::new();
    let mut eager: HashMap<u32, Expression> = HashMap::new();
    let mut existing_names: HashSet<String> = HashSet::new();
    for &fid in members {
        let Some(stmts) = all_ir.get(&fid) else { continue };
        collect_existing_binding_names(stmts, &mut existing_names);
        let aliases = loader_aliases(stmts);
        if let Some(id) = lazy_getter_module(stmts, &aliases, deps) {
            lazy_ids.insert(id);
            continue;
        }
        for (id, callee) in loader_uses(stmts, &aliases, deps) {
            eager.entry(id).or_insert(callee);
        }
    }

    // Hoist only ids used purely eagerly, so no lazy getter is forced eager.
    let mut hoist: Vec<(u32, Expression)> = eager
        .into_iter()
        .filter(|(id, _)| !lazy_ids.contains(id))
        .collect();
    if hoist.is_empty() {
        return;
    }
    hoist.sort_by_key(|(id, _)| *id);

    // Binding name per module id: reuse an existing module-scope binding, else the
    // inferred module name when it is a free identifier, else `_mod{N}`.
    let mut bindings: BTreeMap<u32, String> = BTreeMap::new();
    let mut inject: Vec<(u32, Expression)> = Vec::new();
    let mut used: HashSet<String> = existing_names;
    for (id, callee) in &hoist {
        if let Some(name) = existing.get(id) {
            bindings.insert(*id, name.clone());
            continue;
        }
        let base = registry
            .modules
            .get(id)
            .and_then(|m| m.name.clone())
            .map(|n| sanitize_identifier(&n))
            .filter(|n| !n.is_empty() && n != "default" && !used.contains(n));
        let name = base.unwrap_or_else(|| {
            let mut n = format!("_mod{id}");
            while used.contains(&n) {
                n.push('_');
            }
            n
        });
        used.insert(name.clone());
        bindings.insert(*id, name);
        inject.push((*id, callee.clone()));
    }

    // Rewrite every eager loader call across the module to its binding.
    let hoist_ids: BTreeSet<u32> = hoist.iter().map(|(id, _)| *id).collect();
    for &fid in members {
        if let Some(stmts) = all_ir.get_mut(&fid) {
            let aliases = loader_aliases(stmts);
            if lazy_getter_module(stmts, &aliases, deps).is_some() {
                continue;
            }
            let mut rw = LoaderRewriter { bindings: &bindings, aliases: &aliases, ids: &hoist_ids, deps };
            if fid == factory_id && !existing.is_empty() {
                // Skip the reused binding's own statement, otherwise its RHS loader
                // call is rewritten to `let x = x` and the import source is lost.
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

    // Inject `const name = loader(id)` for the ids without an existing binding, at
    // the top of the factory, so the ESM classifier lifts them into imports.
    if !inject.is_empty() {
        if let Some(factory) = all_ir.get_mut(&factory_id) {
            let mut injected: Vec<Statement> = Vec::new();
            for (id, callee) in &inject {
                let name = bindings.get(id).cloned().unwrap_or_default();
                injected.push(Statement::Let {
                    name,
                    value: Expression::Call {
                        callee: Box::new(callee.clone()),
                        arguments: vec![Expression::Value(Value::Constant(Constant::Integer(*id as i32)))],
                    },
                    kind: VarKind::Const,
                });
            }
            injected.extend(std::mem::take(factory));
            *factory = injected;
        }
    }
}

// True when `stmt` is exactly the reused module binding `let name = loader(id)`
// whose name we adopted, so it must be left untouched during rewriting.
fn is_reused_binding(
    stmt: &Statement,
    existing: &HashMap<u32, String>,
    aliases: &HashSet<String>,
    deps: &[u32],
) -> bool {
    if let Statement::Let { name, value, .. } = stmt {
        if let Some((id, _)) = loader_call(value, aliases, deps) {
            return existing.get(&id) == Some(name);
        }
    }
    false
}

// Collect names bound by `let`/`const` in a body.
fn collect_existing_binding_names(stmts: &[Statement], out: &mut HashSet<String>) {
    for s in stmts {
        if let Statement::Let { name, .. } = s {
            out.insert(name.clone());
        }
    }
}

// Names that hold a loader within a body: the built-in loader names plus any local
// assigned one of them (`tmp4 = require`), to a fixed point.
fn loader_aliases(stmts: &[Statement]) -> HashSet<String> {
    let mut set: HashSet<String> = LOADER_NAMES.iter().map(|s| s.to_string()).collect();
    let mut changed = true;
    while changed {
        changed = false;
        for s in stmts {
            let (name, value) = match s {
                Statement::Let { name, value, .. } => (name, value),
                Statement::Assign { target: crate::ir::AssignTarget::Variable(name), value } => (name, value),
                _ => continue,
            };
            if let Expression::Value(Value::Variable(v)) = value {
                if set.contains(v) && set.insert(name.clone()) {
                    changed = true;
                }
            }
        }
    }
    set
}

// Resolve a loader call argument to an absolute module id: `loader(1234)` -> 1234,
// `loader(dependencyMap[i])` -> deps[i].
fn resolve_module_id(arg: &Expression, deps: &[u32]) -> Option<u32> {
    match arg {
        Expression::Value(Value::Constant(Constant::Integer(n))) => Some(*n as u32),
        Expression::Member { property: PropertyKey::Index(i), .. } => deps.get(*i as usize).copied(),
        Expression::Member { property: PropertyKey::Computed(k), .. } => {
            if let Expression::Value(Value::Constant(Constant::Integer(i))) = k.as_ref() {
                deps.get(*i as usize).copied()
            } else {
                None
            }
        }
        _ => None,
    }
}

// If `stmts` is a lazy loader getter (a body whose only real statement returns a
// single loader access), return the loaded module id.
fn lazy_getter_module(stmts: &[Statement], aliases: &HashSet<String>, deps: &[u32]) -> Option<u32> {
    let real: Vec<&Statement> = stmts.iter().filter(|s| !matches!(s, Statement::Comment(_))).collect();
    if real.len() != 1 {
        return None;
    }
    let Statement::Return(Some(expr)) = real[0] else {
        return None;
    };
    let mut ids = Vec::new();
    collect_loader_ids(expr, aliases, deps, &mut ids);
    if ids.len() == 1 {
        Some(ids[0])
    } else {
        None
    }
}

// Eager loader uses in a body: (module id, loader callee).
fn loader_uses(stmts: &[Statement], aliases: &HashSet<String>, deps: &[u32]) -> Vec<(u32, Expression)> {
    struct C<'a> {
        aliases: &'a HashSet<String>,
        deps: &'a [u32],
        out: Vec<(u32, Expression)>,
    }
    impl Visitor<'_> for C<'_> {
        fn visit_expression(&mut self, e: &Expression) {
            if let Some((id, callee)) = loader_call(e, self.aliases, self.deps) {
                self.out.push((id, callee.clone()));
            }
            self.walk_expression(e);
        }
    }
    let mut c = C { aliases, deps, out: Vec::new() };
    for s in stmts {
        c.visit_statement(s);
    }
    c.out
}

fn collect_loader_ids(expr: &Expression, aliases: &HashSet<String>, deps: &[u32], out: &mut Vec<u32>) {
    struct C<'a> {
        aliases: &'a HashSet<String>,
        deps: &'a [u32],
        out: &'a mut Vec<u32>,
    }
    impl Visitor<'_> for C<'_> {
        fn visit_expression(&mut self, e: &Expression) {
            if let Some((id, _)) = loader_call(e, self.aliases, self.deps) {
                self.out.push(id);
            }
            self.walk_expression(e);
        }
    }
    let mut c = C { aliases, deps, out };
    c.visit_expression(expr);
}

// If `expr` is `LOADER(id)` with a recognized loader callee, return (module id, callee).
fn loader_call<'a>(expr: &'a Expression, aliases: &HashSet<String>, deps: &[u32]) -> Option<(u32, &'a Expression)> {
    let Expression::Call { callee, arguments } = expr else {
        return None;
    };
    let name = match callee.as_ref() {
        Expression::Value(Value::Variable(n)) => n.as_str(),
        _ => return None,
    };
    if !aliases.contains(name) {
        return None;
    }
    let id_arg = match arguments.first() {
        Some(Expression::Value(Value::Constant(Constant::Undefined))) => arguments.get(1),
        other => other,
    }?;
    resolve_module_id(id_arg, deps).map(|id| (id, callee.as_ref()))
}

// Rewrite each eager loader call whose id is hoisted into a reference to its binding.
struct LoaderRewriter<'a> {
    bindings: &'a BTreeMap<u32, String>,
    aliases: &'a HashSet<String>,
    ids: &'a BTreeSet<u32>,
    deps: &'a [u32],
}
impl MutVisitor for LoaderRewriter<'_> {
    fn visit_expression(&mut self, e: &mut Expression) {
        if let Some((id, _)) = loader_call(e, self.aliases, self.deps) {
            if self.ids.contains(&id) {
                if let Some(name) = self.bindings.get(&id) {
                    *e = Expression::Value(Value::Variable(name.clone()));
                    return;
                }
            }
        }
        self.walk_expression(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(n: i32) -> Expression {
        Expression::Value(Value::Constant(Constant::Integer(n)))
    }
    fn var(n: &str) -> Expression {
        Expression::Value(Value::Variable(n.to_string()))
    }
    fn call(callee: &str, arg: Expression) -> Expression {
        Expression::Call { callee: Box::new(var(callee)), arguments: vec![arg] }
    }

    #[test]
    fn aliases_follow_require_copies() {
        let stmts = vec![
            Statement::Assign {
                target: crate::ir::AssignTarget::Variable("tmp4".into()),
                value: var("require"),
            },
            Statement::Let { name: "r2".into(), value: var("tmp4"), kind: VarKind::Const },
        ];
        let a = loader_aliases(&stmts);
        assert!(a.contains("tmp4"));
        assert!(a.contains("r2"));
        assert!(a.contains("importDefault"));
    }

    #[test]
    fn resolves_integer_and_dependency_map() {
        let deps = vec![100, 200, 300];
        assert_eq!(resolve_module_id(&int(509), &deps), Some(509));
        let dm = Expression::Member {
            object: Box::new(var("dependencyMap")),
            property: PropertyKey::Index(1),
            optional: false,
        };
        assert_eq!(resolve_module_id(&dm, &deps), Some(200));
    }

    #[test]
    fn recognizes_import_default_loader() {
        let aliases = loader_aliases(&[]);
        let e = call("importDefault", int(530));
        assert_eq!(loader_call(&e, &aliases, &[]).map(|(id, _)| id), Some(530));
    }

    #[test]
    fn detects_lazy_getter_and_ignores_eager_body() {
        let aliases = loader_aliases(&[]);
        // A getter arrow: single `return require(345).default`.
        let getter = vec![Statement::Return(Some(Expression::Member {
            object: Box::new(call("require", int(345))),
            property: PropertyKey::Ident("default".into()),
            optional: false,
        }))];
        assert_eq!(lazy_getter_module(&getter, &aliases, &[]), Some(345));

        // An eager body with two statements is not a lazy getter.
        let eager = vec![
            Statement::Let { name: "a".into(), value: call("require", int(345)), kind: VarKind::Const },
            Statement::Return(Some(var("a"))),
        ];
        assert_eq!(lazy_getter_module(&eager, &aliases, &[]), None);
    }
}
