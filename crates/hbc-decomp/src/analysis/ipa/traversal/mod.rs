// IPA collection pass: per function, build the local definition maps (what each
// register/variable holds, and the raw assigned expression), then run the visitor
// that links the call graph and gathers parameter naming hints.

use super::graph::CallGraph;
use super::resolution::{get_base_name, FunctionNameIndex};
use crate::analysis::dataflow::reaching_bindings::{Defs, Reach, ReachingDefinitions};
use crate::analysis::metro::registry::MetroRegistry;
use crate::ir::extract_function_id;
use crate::ir::{target_to_key, Expression, PropertyKey, Statement, Value};
use std::collections::BTreeMap;
use std::collections::HashMap;

mod visitor;

// How call resolution asks what a name is currently defined as. A closure rather
// than a map so the flow sensitive caller can answer from the fact holding at the
// statement being visited, without rebuilding a map at every program point.
pub(super) type DefLookup<'a> = &'a dyn Fn(&str) -> Option<Definition>;

#[derive(Clone, PartialEq)]
pub(super) enum Definition {
    Function(u32),
    Parameter(u32),
    Module(u32),
    RequireAlias,
    // The register or variable holds the global object (`globalThis`, from
    // GetGlobalObject). Used to recognise `r1 = globalThis.foo` below.
    Global,
    // The register or variable holds a property read off the global object, e.g.
    // `r1 = globalThis.mid`. The string is the property name, later resolved to a
    // function id through the name index so an indirect `r1(...)` call still links.
    GlobalMember(String),
}

pub struct CollectContext<'a> {
    pub graph: &'a mut CallGraph,
    pub call_sites: &'a mut BTreeMap<u32, Vec<Vec<Option<String>>>>,
    pub self_param_names: &'a mut BTreeMap<u32, Vec<Vec<Option<String>>>>,
    pub param_links: &'a mut Vec<super::structs::ParamLink>,
    pub metro_registry: &'a MetroRegistry,
    pub func_name_index: &'a FunctionNameIndex,
}

pub fn collect_info(caller_id: u32, stmts: &[Statement], ctx: &mut CollectContext<'_>) {
    let mut defs = HashMap::new();
    for stmt in stmts {
        collect_definitions(stmt, &mut defs);
    }
    add_definitions_the_tree_walk_cannot_reach(stmts, &mut defs);

    // Local register/variable definitions (the raw assigned expression) so a
    // call-site hint can be followed through the temporaries IPA sees before the
    // pipeline inlines them (`r5 = first.join(""); f(email, r5)`).
    let mut value_defs = HashMap::new();
    for stmt in stmts {
        collect_value_exprs(stmt, &mut value_defs);
    }

    // What a name is defined as depends on where in the function the question is
    // asked. Hermes reuses one register for unrelated values all the time, so a
    // single map for the whole body answers with whichever definition a tree walk
    // happened to visit last, and every call site but one gets a wrong answer.
    let extract =
        |stmt: &Statement, fact: &Defs<Definition>| -> Option<(String, Option<Definition>)> {
            let (key, value) = match stmt {
                Statement::Let { name, value, .. } => (name.clone(), value),
                Statement::Assign { target, value } => (target_to_key(target)?, value),
                _ => return None,
            };
            let lookup = |name: &str| fact.get(name).cloned();
            Some((key, summarise_value(value, &lookup)))
        };
    let analysis = ReachingDefinitions::new(&extract);

    // A function declaration is reachable before the statement that defines it, so
    // names whose only definition anywhere in the body is one function id start out
    // already bound. Anything defined more than one way is left to the flow.
    let entry = hoisted_function_definitions(stmts);

    // The flow refines the whole-body map rather than replacing it. Where the flow
    // has a single definition in force it is the precise answer and wins. Where it
    // has two, the name is genuinely ambiguous there and resolving it would be a
    // guess, so the call stays unresolved. Where it has nothing to say, the map
    // answers as it always did, which keeps every resolution that does not depend
    // on position, notably the ones reached through a path the flow prunes.
    let mut visit = |stmt: &Statement, fact: &Defs<Definition>| {
        let lookup = |name: &str| match fact.0.get(name) {
            Some(Reach::One(def)) => Some(def.clone()),
            Some(Reach::Many) => defs.get(name).cloned(),
            None => defs.get(name).cloned(),
        };
        visitor::run_on(caller_id, stmt, &lookup, &value_defs, ctx);
    };
    crate::analysis::dataflow::solve_observed(&analysis, stmts, entry, &mut visit);
}

// Names the body defines exactly one way, and that way is a function id. Function
// declarations are hoisted in JavaScript, so a call that appears above the
// definition still reaches it, and the flow would otherwise report the name as
// undefined at that point.
fn hoisted_function_definitions(stmts: &[Statement]) -> Defs<Definition> {
    use crate::analysis::dataflow::reaching_bindings::Reach;

    let mut seen: HashMap<String, Option<Definition>> = HashMap::new();
    let mut record = |key: String, def: Option<Definition>| match seen.get(&key) {
        Some(existing) if *existing == def => {}
        Some(_) => {
            seen.insert(key, None);
        }
        None => {
            seen.insert(key, def);
        }
    };
    let empty = Defs::new();
    let lookup = |name: &str| empty.get(name).cloned();
    let mut walk = |stmt: &Statement| {
        if let Some((key, value)) = match stmt {
            Statement::Let { name, value, .. } => Some((name.clone(), value)),
            Statement::Assign { target, value } => target_to_key(target).map(|k| (k, value)),
            _ => None,
        } {
            record(key, summarise_value(value, &lookup));
        }
    };
    walk_every_statement(stmts, &mut walk);

    let mut out = Defs::new();
    for (key, def) in seen {
        if let Some(Definition::Function(fid)) = def {
            out.0.insert(key, Reach::One(Definition::Function(fid)));
        }
    }
    out
}

fn walk_every_statement(stmts: &[Statement], f: &mut impl FnMut(&Statement)) {
    for stmt in stmts {
        f(stmt);
        crate::ir::for_each_nested_body(stmt, &mut |body| walk_every_statement(body, f));
    }
}

// `collect_definitions` only descends into `Block` and `If`, so a binding defined
// inside a loop, a `switch`, a `try` or a `for ... of` body is invisible to call
// resolution and every call through it stays unresolved, which is one of the ways
// a parameter keeps its `argN` name.
//
// The dataflow engine walks the whole structure and reports a name as defined only
// when a single definition reaches the end of the function, so what is added here
// is never a guess between two candidates. Names the tree walk already resolved are
// left alone: this only fills gaps.
fn add_definitions_the_tree_walk_cannot_reach(
    stmts: &[Statement],
    defs: &mut HashMap<String, Definition>,
) {
    use crate::analysis::dataflow::reaching_bindings::{Defs, Reach, ReachingDefinitions};

    let extract =
        |stmt: &Statement, fact: &Defs<Definition>| -> Option<(String, Option<Definition>)> {
            let (key, value) = match stmt {
                Statement::Let { name, value, .. } => (name.clone(), value),
                Statement::Assign { target, value } => (target_to_key(target)?, value),
                _ => return None,
            };
            let lookup = |name: &str| fact.get(name).cloned();
            Some((key, summarise_value(value, &lookup)))
        };
    let analysis = ReachingDefinitions::new(&extract);
    let reached = crate::analysis::dataflow::solve(&analysis, stmts, Defs::new());

    for (key, def) in reached.0 {
        if let Reach::One(def) = def {
            defs.entry(key).or_insert(def);
        }
    }
}

fn collect_definitions(stmt: &Statement, defs: &mut HashMap<String, Definition>) {
    match stmt {
        Statement::Assign { target, value } => {
            if let Some(key) = target_to_key(target) {
                collect_value_definition(&key, value, defs);
            }
        }
        Statement::Let { name, value, .. } => {
            collect_value_definition(name, value, defs);
        }
        Statement::Block(stmts) => {
            for s in stmts {
                collect_definitions(s, defs);
            }
        }
        Statement::If {
            then_body,
            else_body,
            ..
        } => {
            for s in then_body {
                collect_definitions(s, defs);
            }
            for s in else_body {
                collect_definitions(s, defs);
            }
        }
        _ => {}
    }
}

// Record `target = <expr>` for register/variable targets so a call-site hint can
// be followed to its source. Mirrors `collect_definitions` statement coverage and
// its flat last-definition-wins behaviour.
fn collect_value_exprs(stmt: &Statement, out: &mut HashMap<String, Expression>) {
    match stmt {
        Statement::Assign { target, value } => {
            if let Some(key) = target_to_key(target) {
                out.insert(key, value.clone());
            }
        }
        Statement::Let { name, value, .. } => {
            out.insert(name.clone(), value.clone());
        }
        Statement::Block(stmts) => {
            for s in stmts {
                collect_value_exprs(s, out);
            }
        }
        Statement::If {
            then_body,
            else_body,
            ..
        } => {
            for s in then_body {
                collect_value_exprs(s, out);
            }
            for s in else_body {
                collect_value_exprs(s, out);
            }
        }
        _ => {}
    }
}

fn collect_value_definition(key: &str, value: &Expression, defs: &mut HashMap<String, Definition>) {
    let lookup = |name: &str| defs.get(name).cloned();
    if let Some(def) = summarise_value(value, &lookup) {
        defs.insert(key.to_string(), def);
    }
}

// What a value assigned to a binding is, as far as call resolution is concerned.
// Returns `None` when the value carries nothing resolvable, which a flow sensitive
// caller reads as "this write invalidates whatever was known about the name".
//
// `lookup` answers what a name is currently defined as. It is a closure rather
// than the map itself so the dataflow client can answer from its own fact without
// rebuilding a map at every statement.
pub(super) fn summarise_value(
    value: &Expression,
    lookup: &dyn Fn(&str) -> Option<Definition>,
) -> Option<Definition> {
    if let Expression::Value(Value::Binding(crate::ir::Binding::Variable(name))) = value {
        if is_known_require_name(name) || matches!(lookup(name), Some(Definition::RequireAlias)) {
            return Some(Definition::RequireAlias);
        }
    }

    if let Some(fid) = extract_function_id(value) {
        return Some(Definition::Function(fid));
    }
    if let Some(mod_id) = extract_require_call_with(value, lookup) {
        return Some(Definition::Module(mod_id));
    }
    if let Expression::Value(Value::Parameter(idx)) = value {
        return Some(Definition::Parameter(*idx));
    }
    if let Expression::Value(Value::Global) = value {
        return Some(Definition::Global);
    }

    if let Expression::Member {
        object, property, ..
    } = value
    {
        let prop_name = match property {
            PropertyKey::String(p) | PropertyKey::Ident(p) => Some(p.as_str()),
            _ => None,
        };
        let mut found: Option<Definition> = None;
        // `x = y.default` where y is a module keeps pointing at that module.
        if let Some(base) = get_base_name(object) {
            if let Some(Definition::Module(mod_id)) = lookup(&base) {
                if prop_name == Some("default") {
                    found = Some(Definition::Module(mod_id));
                }
            }
        }
        // Track `x = globalThis.foo` so an indirect `x(...)` call resolves through
        // the function name index. The base is global either directly
        // (Value::Global) or through a register that earlier held GetGlobalObject.
        let base_is_global = matches!(object.as_ref(), Expression::Value(Value::Global))
            || get_base_name(object)
                .and_then(|b| lookup(&b))
                .is_some_and(|d| matches!(d, Definition::Global));
        if base_is_global {
            if let Some(prop) = prop_name {
                found = Some(Definition::GlobalMember(prop.to_string()));
            }
        }
        // `x = arg0.value` still comes from arg0.
        if let Expression::Value(Value::Parameter(idx)) = object.as_ref() {
            found = Some(Definition::Parameter(*idx));
        }
        return found;
    }

    if let Expression::Call { arguments, .. } = value {
        if arguments.len() == 1 {
            if let Expression::Value(Value::Parameter(idx)) = &arguments[0] {
                return Some(Definition::Parameter(*idx));
            }
        }
    }

    None
}

// Extract the required module ID from a `require` call.
fn extract_require_call_with(
    expr: &Expression,
    lookup: &dyn Fn(&str) -> Option<Definition>,
) -> Option<u32> {
    if let Expression::Call { callee, arguments } = expr {
        // Hermes prepends `this` (often undefined) so require(id) is Call2.
        let id_arg = match arguments.len() {
            1 => arguments.first()?,
            2 => arguments.get(1)?,
            _ => return None,
        };
        if let Expression::Value(Value::Constant(crate::ir::Constant::Integer(n))) = id_arg {
            match callee.as_ref() {
                Expression::Value(Value::Binding(crate::ir::Binding::Variable(name)))
                    if is_known_require_name(name)
                        || matches!(lookup(name), Some(Definition::RequireAlias)) =>
                {
                    return Some(*n as u32)
                }
                Expression::Value(Value::Binding(crate::ir::Binding::Register(r)))
                    if matches!(lookup(&format!("r{r}")), Some(Definition::RequireAlias)) =>
                {
                    return Some(*n as u32)
                }
                _ => {}
            }
        }
    }
    None
}

fn is_known_require_name(name: &str) -> bool {
    crate::analysis::metro::registry::FactoryRoles::matches_require_loader_name(name)
}

// A short readable label for a call's callee, for `--log ipa=trace`. Renders the
// method or function name (and the base of a member chain) so a specific call like
// `checkProfile.default.loginWithToken(...)` is greppable in the trace.
fn callee_trace_name(callee: &Expression) -> String {
    match callee {
        Expression::Value(Value::Binding(crate::ir::Binding::Variable(n))) => format!("{n}()"),
        Expression::Value(Value::Binding(crate::ir::Binding::Register(r))) => format!("r{r}()"),
        Expression::Function { id, .. } => format!("fn{}()", id.0),
        Expression::Member {
            object, property, ..
        } => {
            let prop = match property {
                PropertyKey::String(s) | PropertyKey::Ident(s) => s.clone(),
                PropertyKey::Index(i) => format!("[{i}]"),
                PropertyKey::Computed(_) => "[computed]".to_string(),
            };
            match object.as_ref() {
                Expression::Value(Value::Binding(crate::ir::Binding::Variable(n))) => {
                    format!("{n}.{prop}()")
                }
                Expression::Member {
                    property: PropertyKey::String(b) | PropertyKey::Ident(b),
                    ..
                } => {
                    format!("{b}.{prop}()")
                }
                _ => format!("?.{prop}()"),
            }
        }
        _ => "?()".to_string(),
    }
}

// Synthetic parameter name hints for callbacks passed to well-known array/promise
// and DOM methods. Conventional, so treated as hints that voting can override.
fn callback_param_hints(method: &str) -> Option<Vec<Option<String>>> {
    match method {
        "map" | "filter" | "find" | "some" | "every" | "forEach" | "findIndex" | "flatMap" => {
            Some(vec![Some("item".to_string()), Some("index".to_string())])
        }
        "reduce" | "reduceRight" => Some(vec![
            Some("acc".to_string()),
            Some("item".to_string()),
            Some("index".to_string()),
        ]),
        "sort" => Some(vec![Some("a".to_string()), Some("b".to_string())]),
        "then" => Some(vec![Some("result".to_string())]),
        "catch" => Some(vec![Some("error".to_string())]),
        // Only DOM/RN `addEventListener` reliably passes an event. `.on`/`.addListener`
        // on a custom emitter usually pass a payload/value, so naming that param
        // `event` would be a wrong guess.
        "addEventListener" => Some(vec![Some("event".to_string())]),
        "replace" | "replaceAll" => {
            Some(vec![Some("match_".to_string()), Some("offset".to_string())])
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
