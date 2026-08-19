// The IPA expression visitor: walks a function body, links the call graph, and
// collects per-parameter naming hints from call sites (plus synthetic hints for
// callbacks passed to well-known array/promise methods).

use std::collections::{BTreeMap, HashMap};

use super::super::inference::collect_param_names_from_expr;
use super::super::resolution::{
    extract_method_object_name, extract_name_from_callee, extract_object_name_from_method_call,
    resolve_callee, FunctionNameIndex,
};
use super::super::structs::ParamLink;
use super::{CollectContext, Definition};
use crate::analysis::ipa::graph::CallGraph;
use crate::analysis::metro::registry::MetroRegistry;
use crate::ir::{extract_function_id, Expression, PropertyKey, Value};

// Build the visitor for one function and walk its statements. Kept here so the
// `IpaVisitor` type and its fields stay private to this module.
pub(super) fn run(
    caller_id: u32,
    stmts: &[crate::ir::Statement],
    defs: &HashMap<String, Definition>,
    value_defs: &HashMap<String, Expression>,
    ctx: &mut CollectContext<'_>,
) {
    use crate::ir::Visitor;
    let mut visitor = IpaVisitor {
        caller_id,
        defs,
        value_defs,
        graph: ctx.graph,
        call_sites: ctx.call_sites,
        self_param_names: ctx.self_param_names,
        param_links: ctx.param_links,
        metro_registry: ctx.metro_registry,
        func_name_index: ctx.func_name_index,
    };
    for stmt in stmts {
        visitor.visit_statement(stmt);
    }
}

// The caller parameter an argument forwards directly (`f(arg0)`), for cross-function
// name propagation. Only a direct forward links: propagating a name through a
// transformation such as `f(first.join(""))` would name the callee parameter after
// the source array, which is not what the parameter actually holds. Those
// transformed cases are handled later by data-flow grounding, not by a raw link.
fn param_forwarded_by_arg(arg: &Expression, defs: &HashMap<String, Definition>) -> Option<u32> {
    match arg {
        Expression::Value(Value::Parameter(idx)) => Some(*idx),
        Expression::Value(Value::Variable(name)) => param_index_of(name, defs),
        Expression::Value(Value::Register(r)) => param_index_of(&format!("r{r}"), defs),
        _ => None,
    }
}

fn param_index_of(key: &str, defs: &HashMap<String, Definition>) -> Option<u32> {
    match defs.get(key) {
        Some(Definition::Parameter(idx)) => Some(*idx),
        _ => None,
    }
}

use super::{callback_param_hints, callee_trace_name};

struct IpaVisitor<'a> {
    value_defs: &'a HashMap<String, Expression>,
    caller_id: u32,
    defs: &'a HashMap<String, Definition>,
    graph: &'a mut CallGraph,
    call_sites: &'a mut BTreeMap<u32, Vec<Vec<Option<String>>>>,
    self_param_names: &'a mut BTreeMap<u32, Vec<Vec<Option<String>>>>,
    param_links: &'a mut Vec<ParamLink>,
    metro_registry: &'a MetroRegistry,
    func_name_index: &'a FunctionNameIndex,
}

impl IpaVisitor<'_> {
    // Resolve a callback expression to its function ID.
    fn resolve_callback_id(&self, expr: &Expression) -> Option<u32> {
        // Direct function expression
        if let Some(fid) = extract_function_id(expr) {
            return Some(fid);
        }
        // Variable reference to a known function
        if let Expression::Value(Value::Variable(name)) = expr {
            if let Some(Definition::Function(fid)) = self.defs.get(name) {
                return Some(*fid);
            }
        }
        // Register reference to a known function
        if let Expression::Value(Value::Register(r)) = expr {
            if let Some(Definition::Function(fid)) = self.defs.get(&format!("r{r}")) {
                return Some(*fid);
            }
        }
        None
    }
}

impl<'a> crate::ir::Visitor<'a> for IpaVisitor<'a> {
    fn visit_expression(&mut self, expr: &'a Expression) {
        collect_param_names_from_expr(expr, self.caller_id, self.self_param_names);

        match expr {
            Expression::Call { callee, arguments } => {
                let callee_id = resolve_callee(callee, self.defs, self.metro_registry, self.func_name_index);

                // Trace how each call site resolves. An UNRESOLVED method call is
                // the usual reason a parameter keeps its `argN` name: the call site
                // that carries the naming hint never gets attributed to the callee.
                if log::log_enabled!(target: "ipa", log::Level::Trace) {
                    match callee_id {
                        Some(id) => log::trace!(
                            target: "ipa",
                            "call {} -> fn{id} (from fn{})",
                            callee_trace_name(callee), self.caller_id
                        ),
                        None => log::trace!(
                            target: "ipa",
                            "call {} UNRESOLVED (from fn{})",
                            callee_trace_name(callee), self.caller_id
                        ),
                    }
                }

                if let Some(id) = callee_id {
                    self.graph.add_call(self.caller_id, id);

                    // IPA runs before `strip_hermes_this` (stage W12), so every
                    // Call still carries the Hermes `this` slot at arguments[0]
                    // (the receiver object for method calls, `undefined` for
                    // plain calls). Drop it so argument positions are user-0-
                    // indexed, matching how the callee body names its parameters
                    // (LoadParam idx to Parameter(idx-1) to argN). Without this,
                    // plain calls were this-indexed while body/self hints were
                    // user-indexed, producing a spurious trailing param slot.
                    let args_to_process: &[Expression] = if !arguments.is_empty() {
                        &arguments[1..]
                    } else {
                        arguments
                    };

                    let mut arg_names = Vec::new();
                    for (arg_idx, arg) in args_to_process.iter().enumerate() {
                        // A parameter link is created only when an argument forwards
                        // a parameter directly (`f(arg0)` / a variable defined as a
                        // parameter), so the callee position inherits the caller name.
                        let resolved_param = param_forwarded_by_arg(arg, self.defs);

                        // Derive the naming hint, following local temporaries to
                        // their source so an intermediate register does not erase it.
                        arg_names.push(crate::analysis::ipa::arg_hints::hint_from_arg(arg, self.value_defs));

                        if let Some(src_idx) = resolved_param {
                            self.param_links.push(ParamLink { src_func: self.caller_id, src_param: src_idx, dst_func: id, dst_param: arg_idx as u32 });
                        }
                    }

                    if !arg_names.is_empty() {
                        log::trace!(target: "ipa", "  hints fn{id}: {arg_names:?}");
                        self.call_sites.entry(id).or_default().push(arg_names);
                    }
                }

                // Inject synthetic call-site hints for callbacks passed to
                // well-known methods (`.map`/`.then`/`.sort`/...). This runs
                // regardless of whether the method itself resolved to a user
                // function id: array/promise methods are builtins that never
                // resolve, yet their callback argument is still a nameable user
                // function. Only the method name and the callback argument are
                // needed here.
                if let Expression::Member { property: PropertyKey::Ident(method), .. } = callee.as_ref() {
                    if let Some(hints) = callback_param_hints(method) {
                        // These are all method calls, so index into the
                        // this-stripped user arguments (slot 0 of the raw
                        // `arguments` is the Hermes `this` receiver), otherwise
                        // `data.map(cb)` would inspect `data` instead of `cb`.
                        let user_args: &[Expression] = if !arguments.is_empty() {
                            &arguments[1..]
                        } else {
                            arguments
                        };
                        let cb_arg_idx = match method.as_str() {
                            "addEventListener" | "on" | "addListener" => 1,
                            _ => 0,
                        };
                        if let Some(cb_arg) = user_args.get(cb_arg_idx) {
                            if let Some(cb_id) = self.resolve_callback_id(cb_arg) {
                                self.call_sites.entry(cb_id).or_default().push(hints);
                            }
                        }
                    }
                }
            }
            Expression::New { callee, arguments } => self.collect_new_hints(callee, arguments),
            _ => {}
        }

        self.walk_expression(expr);
    }
}

impl IpaVisitor<'_> {
    // Constructor call `new C(args)`: link the call graph and collect argument name
    // hints. Constructor arguments carry no Hermes `this` slot, so they are already
    // user-0-indexed (unlike the method-call path above).
    fn collect_new_hints(&mut self, callee: &Expression, arguments: &[Expression]) {
        let Some(id) = resolve_callee(callee, self.defs, self.metro_registry, self.func_name_index)
        else {
            return;
        };
        self.graph.add_call(self.caller_id, id);

        let mut arg_names = Vec::new();
        for (arg_idx, arg) in arguments.iter().enumerate() {
            let mut resolved_param = None;

            match arg {
                Expression::Value(Value::Variable(name)) => {
                    if let Some(Definition::Parameter(idx)) = self.defs.get(name) {
                        resolved_param = Some(*idx);
                    }
                    arg_names.push(Some(name.clone()));
                }
                Expression::Value(Value::Register(r)) => {
                    let r_name = format!("r{r}");
                    if let Some(Definition::Parameter(idx)) = self.defs.get(&r_name) {
                        resolved_param = Some(*idx);
                    }
                    arg_names.push(Some(r_name));
                }
                Expression::Value(Value::Parameter(src_idx)) => {
                    resolved_param = Some(*src_idx);
                    arg_names.push(None);
                }
                Expression::Value(Value::Constant(crate::ir::Constant::String(s))) => {
                    if s.chars().all(|c| c.is_alphanumeric() || c == '_') && !s.is_empty() {
                        arg_names.push(Some(s.clone()));
                    } else {
                        arg_names.push(None);
                    }
                }
                Expression::Member { property: PropertyKey::String(prop), .. }
                | Expression::Member { property: PropertyKey::Ident(prop), .. } => {
                    arg_names.push(Some(prop.clone()));
                }
                Expression::Call { callee: inner_callee, .. } => {
                    if let Some(name) = extract_name_from_callee(inner_callee) {
                        arg_names.push(Some(name));
                    } else if let Some(name) = extract_object_name_from_method_call(inner_callee) {
                        arg_names.push(Some(name));
                    } else if let Some(name) = extract_method_object_name(inner_callee) {
                        // `foo(tokens.join(""))` names the param after `tokens`
                        arg_names.push(Some(name));
                    } else {
                        arg_names.push(None);
                    }
                }
                _ => arg_names.push(None),
            }

            if let Some(src_idx) = resolved_param {
                self.param_links.push(ParamLink { src_func: self.caller_id, src_param: src_idx, dst_func: id, dst_param: arg_idx as u32 });
            }
        }

        if !arg_names.is_empty() {
            self.call_sites.entry(id).or_default().push(arg_names);
        }
    }
}
