// IPA collection pass: per function, build the local definition maps (what each
// register/variable holds, and the raw assigned expression), then run the visitor
// that links the call graph and gathers parameter naming hints.

use super::graph::CallGraph;
use super::resolution::{get_base_name, FunctionNameIndex};
use crate::analysis::metro::registry::MetroRegistry;
use crate::ir::extract_function_id;
use crate::ir::{target_to_key, Expression, PropertyKey, Statement, Value};
use std::collections::BTreeMap;
use std::collections::HashMap;

mod visitor;

#[derive(Clone)]
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

    // Local register/variable definitions (the raw assigned expression) so a
    // call-site hint can be followed through the temporaries IPA sees before the
    // pipeline inlines them (`r5 = first.join(""); f(email, r5)`).
    let mut value_defs = HashMap::new();
    for stmt in stmts {
        collect_value_exprs(stmt, &mut value_defs);
    }

    visitor::run(caller_id, stmts, &defs, &value_defs, ctx);
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
    if let Expression::Value(Value::Variable(name)) = value {
        if is_known_require_name(name) || matches!(defs.get(name), Some(Definition::RequireAlias)) {
            defs.insert(key.to_string(), Definition::RequireAlias);
            return;
        }
    }

    if let Some(fid) = extract_function_id(value) {
        defs.insert(key.to_string(), Definition::Function(fid));
    } else if let Some(mod_id) = extract_require_call(value, defs) {
        defs.insert(key.to_string(), Definition::Module(mod_id));
    } else if let Expression::Value(Value::Parameter(idx)) = value {
        defs.insert(key.to_string(), Definition::Parameter(*idx));
    } else if let Expression::Value(Value::Global) = value {
        defs.insert(key.to_string(), Definition::Global);
    } else if let Expression::Member {
        object, property, ..
    } = value
    {
        let prop_name = match property {
            PropertyKey::String(p) | PropertyKey::Ident(p) => Some(p.as_str()),
            _ => None,
        };
        // Check for var x = y.default where y is a module
        if let Some(base) = get_base_name(object) {
            if let Some(Definition::Module(mod_id)) = defs.get(&base) {
                if prop_name == Some("default") {
                    defs.insert(key.to_string(), Definition::Module(*mod_id));
                }
            }
        }
        // Track `x = globalThis.foo` so an indirect `x(...)` call resolves through
        // the function name index. The base is global either directly (Value::Global)
        // or through a register that earlier held GetGlobalObject.
        let base_is_global = matches!(object.as_ref(), Expression::Value(Value::Global))
            || get_base_name(object)
                .and_then(|b| defs.get(&b).cloned())
                .is_some_and(|d| matches!(d, Definition::Global));
        if base_is_global {
            if let Some(prop) = prop_name {
                defs.insert(key.to_string(), Definition::GlobalMember(prop.to_string()));
            }
        }
        // Also track if base is a parameter: x = arg0.value -> x comes from arg0
        if let Expression::Value(Value::Parameter(idx)) = object.as_ref() {
            defs.insert(key.to_string(), Definition::Parameter(*idx));
        }
    } else if let Expression::Call { arguments, .. } = value {
        // If call has single param argument, track it
        if arguments.len() == 1 {
            if let Expression::Value(Value::Parameter(idx)) = &arguments[0] {
                defs.insert(key.to_string(), Definition::Parameter(*idx));
            }
        }
    }
}

// Extract the required module ID from a `require` call.
fn extract_require_call(expr: &Expression, defs: &HashMap<String, Definition>) -> Option<u32> {
    if let Expression::Call { callee, arguments } = expr {
        if arguments.len() == 1 {
            if let Expression::Value(Value::Constant(crate::ir::Constant::Integer(n))) =
                arguments[0]
            {
                match callee.as_ref() {
                    Expression::Value(Value::Variable(name))
                        if is_known_require_name(name)
                            || matches!(defs.get(name), Some(Definition::RequireAlias)) =>
                    {
                        return Some(n as u32)
                    }
                    Expression::Value(Value::Register(r))
                        if matches!(defs.get(&format!("r{r}")), Some(Definition::RequireAlias)) =>
                    {
                        return Some(n as u32)
                    }
                    _ => {}
                }
            }
        }
    }
    None
}

fn is_known_require_name(name: &str) -> bool {
    crate::analysis::metro::registry::FactoryRoles::standard().is_require_param(name)
}

// A short readable label for a call's callee, for `--log ipa=trace`. Renders the
// method or function name (and the base of a member chain) so a specific call like
// `checkProfile.default.loginWithToken(...)` is greppable in the trace.
fn callee_trace_name(callee: &Expression) -> String {
    match callee {
        Expression::Value(Value::Variable(n)) => format!("{n}()"),
        Expression::Value(Value::Register(r)) => format!("r{r}()"),
        Expression::Function { id, .. } => format!("fn{}()", id.0),
        Expression::Member { object, property, .. } => {
            let prop = match property {
                PropertyKey::String(s) | PropertyKey::Ident(s) => s.clone(),
                PropertyKey::Index(i) => format!("[{i}]"),
                PropertyKey::Computed(_) => "[computed]".to_string(),
            };
            match object.as_ref() {
                Expression::Value(Value::Variable(n)) => format!("{n}.{prop}()"),
                Expression::Member { property: PropertyKey::String(b) | PropertyKey::Ident(b), .. } => {
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
        "reduce" | "reduceRight" => {
            Some(vec![Some("acc".to_string()), Some("item".to_string()), Some("index".to_string())])
        }
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
