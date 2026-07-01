use super::graph::CallGraph;
use super::inference::collect_param_names_from_expr;
use super::resolution::{extract_name_from_callee, extract_method_object_name, extract_object_name_from_method_call, resolve_callee, get_base_name, FunctionNameIndex};
use crate::analysis::metro::registry::MetroRegistry;
use crate::ir::{target_to_key, Expression, PropertyKey, Statement, Value};
use std::collections::HashMap;
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub(super) enum Definition {
    Function(u32),
    Parameter(u32),
    Module(u32),
    RequireAlias,
}

pub struct CollectContext<'a> {
    pub graph: &'a mut CallGraph,
    pub call_sites: &'a mut BTreeMap<u32, Vec<Vec<Option<String>>>>,
    pub self_param_names: &'a mut BTreeMap<u32, Vec<Vec<Option<String>>>>,
    pub param_links: &'a mut Vec<super::structs::ParamLink>,
    pub metro_registry: &'a MetroRegistry,
    pub func_name_index: &'a FunctionNameIndex,
}

pub fn collect_info(
    caller_id: u32,
    stmts: &[Statement],
    ctx: &mut CollectContext<'_>,
) {
    let mut defs = HashMap::new();
    for stmt in stmts {
        collect_definitions(stmt, &mut defs);
    }

    use crate::ir::Visitor;
    let mut visitor = IpaVisitor {
        caller_id,
        defs: &defs,
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
    } else if let Expression::Member {
        object, property, ..
    } = value
    {
        // Check for var x = y.default where y is a module
        if let Some(base) = get_base_name(object) {
            if let Some(Definition::Module(mod_id)) = defs.get(&base) {
                let prop_name = match property {
                    PropertyKey::String(p) | PropertyKey::Ident(p) => Some(p.as_str()),
                    _ => None,
                };
                if prop_name == Some("default") {
                    defs.insert(key.to_string(), Definition::Module(*mod_id));
                }
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


// Return synthetic parameter names for callbacks passed to well-known array/promise methods.
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
        "addEventListener" | "on" | "addListener" => Some(vec![Some("event".to_string())]),
        "replace" | "replaceAll" => {
            Some(vec![Some("match_".to_string()), Some("offset".to_string())])
        }
        _ => None,
    }
}

struct IpaVisitor<'a> {
    caller_id: u32,
    defs: &'a HashMap<String, Definition>,
    graph: &'a mut CallGraph,
    call_sites: &'a mut BTreeMap<u32, Vec<Vec<Option<String>>>>,
    self_param_names: &'a mut BTreeMap<u32, Vec<Vec<Option<String>>>>,
    param_links: &'a mut Vec<super::structs::ParamLink>,
    metro_registry: &'a MetroRegistry,
    func_name_index: &'a FunctionNameIndex,
}

impl<'a> IpaVisitor<'a> {
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

                if let Some(id) = callee_id {
                    self.graph.add_call(self.caller_id, id);

                    // IPA runs before `strip_hermes_this` (stage W12), so every
                    // Call still carries the Hermes `this` slot at arguments[0]
                    // (the receiver object for method calls, `undefined` for
                    // plain calls). Drop it so argument positions are user-0-
                    // indexed, matching how the callee body names its parameters
                    // (LoadParam idx -> Parameter(idx-1) -> argN). Without this,
                    // plain calls were this-indexed while body/self hints were
                    // user-indexed, producing a spurious trailing param slot.
                    let args_to_process: &[Expression] = if !arguments.is_empty() {
                        &arguments[1..]
                    } else {
                        arguments
                    };

                    let mut arg_names = Vec::new();
                    for (arg_idx, arg) in args_to_process.iter().enumerate() {
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
                                    // `foo(tokens.join(""))` → param named after `tokens`
                                    arg_names.push(Some(name));
                                } else {
                                    arg_names.push(None);
                                }
                            }
                            _ => arg_names.push(None),
                        }

                        if let Some(src_idx) = resolved_param {
                            self.param_links.push(super::structs::ParamLink { src_func: self.caller_id, src_param: src_idx, dst_func: id, dst_param: arg_idx as u32 });
                        }
                    }

                    if !arg_names.is_empty() {
                        self.call_sites.entry(id).or_default().push(arg_names);
                    }

                    // Inject synthetic call site hints for callbacks passed to well-known methods
                    if let Expression::Member { property: PropertyKey::Ident(method), .. } = callee.as_ref() {
                        if let Some(hints) = callback_param_hints(method) {
                            // Determine which argument position contains the callback
                            let cb_arg_idx = match method.as_str() {
                                "addEventListener" | "on" | "addListener" => 1,
                                _ => 0,
                            };
                            if let Some(cb_arg) = arguments.get(cb_arg_idx) {
                                let cb_func_id = self.resolve_callback_id(cb_arg);
                                if let Some(cb_id) = cb_func_id {
                                    self.call_sites.entry(cb_id).or_default().push(hints);
                                }
                            }
                        }
                    }
                }
            }
            Expression::New { callee, arguments } => {
                let callee_id = resolve_callee(callee, self.defs, self.metro_registry, self.func_name_index);

                if let Some(id) = callee_id {
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
                                    // `foo(tokens.join(""))` → param named after `tokens`
                                    arg_names.push(Some(name));
                                } else {
                                    arg_names.push(None);
                                }
                            }
                            _ => arg_names.push(None),
                        }

                        if let Some(src_idx) = resolved_param {
                            self.param_links.push(super::structs::ParamLink { src_func: self.caller_id, src_param: src_idx, dst_func: id, dst_param: arg_idx as u32 });
                        }
                    }

                    if !arg_names.is_empty() {
                        self.call_sites.entry(id).or_default().push(arg_names);
                    }
                }
            }
            _ => {}
        }

        self.walk_expression(expr);
    }
}

use crate::ir::extract_function_id;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_callback_param_hints_map() {
        let hints = callback_param_hints("map").unwrap();
        assert_eq!(hints[0], Some("item".to_string()));
        assert_eq!(hints[1], Some("index".to_string()));
    }

    #[test]
    fn test_callback_param_hints_reduce() {
        let hints = callback_param_hints("reduce").unwrap();
        assert_eq!(hints.len(), 3);
        assert_eq!(hints[0], Some("acc".to_string()));
        assert_eq!(hints[1], Some("item".to_string()));
    }

    #[test]
    fn test_callback_param_hints_then_catch() {
        let then = callback_param_hints("then").unwrap();
        assert_eq!(then[0], Some("result".to_string()));

        let catch = callback_param_hints("catch").unwrap();
        assert_eq!(catch[0], Some("error".to_string()));
    }

    #[test]
    fn test_callback_param_hints_sort() {
        let hints = callback_param_hints("sort").unwrap();
        assert_eq!(hints[0], Some("a".to_string()));
        assert_eq!(hints[1], Some("b".to_string()));
    }

    #[test]
    fn test_callback_param_hints_unknown() {
        assert!(callback_param_hints("unknownMethod").is_none());
    }

    #[test]
    fn test_callback_param_hints_event_listener() {
        let hints = callback_param_hints("addEventListener").unwrap();
        assert_eq!(hints[0], Some("event".to_string()));
    }
}
