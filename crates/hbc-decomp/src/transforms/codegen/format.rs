use super::Codegen;

impl Codegen {
    pub(super) fn format_member_access(&self, obj: &str, opt: &str, key: &crate::ir::PropertyKey) -> String {
        crate::ir::expr::display::format_member_access_with(obj, opt, key, |e| self.generate_expr(e))
    }

    pub(super) fn format_call(&self, callee_str: &str, _callee_expr: Option<&crate::ir::Expression>, arguments: &[crate::ir::Expression], extra_suffix: &str) -> String {
        // After strip_hermes_this() in the pipeline, `this` has already been removed from
        // Call arguments. All remaining arguments are real user-visible arguments.
        format!("{}({}){}", callee_str, self.join_exprs(arguments), extra_suffix)
    }

    pub(super) fn format_property(&self, prop: &crate::ir::ObjectProperty) -> String {
        use crate::ir::{Value, Expression, PropertyKey};

        // Shorthand: { x } instead of { x: x }
        if let PropertyKey::Ident(key_name) = &prop.key {
            if let Expression::Value(Value::Variable(var_name)) = &prop.value {
                if key_name == var_name {
                    return key_name.clone();
                }
            }
        }

        // Method shorthand: { foo() { ... } } instead of { foo: function foo() { ... } }
        if let PropertyKey::Ident(key_name) = &prop.key {
            if let Expression::Function { name: Some(fn_name), is_generator, is_async, .. } = &prop.value {
                if key_name == fn_name {
                    let rendered = self.generate_expr(&prop.value);
                    // Strip "function name" or "async function name" prefix to get method shorthand
                    // e.g. "function get(arg0) { ... }" → "get(arg0) { ... }"
                    // e.g. "async function foo() { ... }" → "async foo() { ... }"
                    let stripped = if *is_async {
                        if let Some(rest) = rendered.strip_prefix("async function* ") {
                            format!("async *{rest}")
                        } else if let Some(rest) = rendered.strip_prefix("async function ") {
                            format!("async {rest}")
                        } else {
                            rendered
                        }
                    } else if *is_generator {
                        if let Some(rest) = rendered.strip_prefix("function* ") {
                            format!("*{rest}")
                        } else {
                            rendered
                        }
                    } else if let Some(rest) = rendered.strip_prefix("function ") {
                        rest.to_string()
                    } else {
                        rendered
                    };
                    return stripped;
                }
            }
        }

        format!("{}: {}", crate::ir::expr::display::format_key(&prop.key), self.generate_expr(&prop.value))
    }

    pub(super) fn generate_assign_target(&self, target: &crate::ir::AssignTarget) -> String {
        use crate::ir::AssignTarget;
        match target {
            AssignTarget::Register(r) => format!("r{r}"),
            AssignTarget::Variable(n) => n.clone(),
            AssignTarget::Member { object, property } => {
                let obj = self.generate_expr(object);
                format!("{obj}.{property}")
            }
            AssignTarget::Index { object, key } => {
                let obj = self.generate_expr(object);
                let k = self.generate_expr(key);
                format!("{obj}[{k}]")
            }
            AssignTarget::ClosureVar { level, slot } => format!("closure_var_{level}_{slot}"),
            AssignTarget::DestructuringArray(elements) => {
                let elems: Vec<String> = elements.iter()
                    .map(|e| e.as_ref().map(|(t, def)| {
                        let t_str = self.generate_assign_target(t);
                        if let Some(d) = def {
                            format!("{} = {}", t_str, self.generate_expr(d))
                        } else {
                            t_str
                        }
                    }).unwrap_or_default())
                    .collect();
                format!("[{}]", elems.join(", "))
            }
            AssignTarget::DestructuringArrayRest { elements, rest } => {
                let mut elems: Vec<String> = elements.iter()
                    .map(|e| e.as_ref().map(|(t, def)| {
                        let t_str = self.generate_assign_target(t);
                        if let Some(d) = def {
                            format!("{} = {}", t_str, self.generate_expr(d))
                        } else {
                            t_str
                        }
                    }).unwrap_or_default())
                    .collect();
                elems.push(format!("...{}", self.generate_assign_target(rest)));
                format!("[{}]", elems.join(", "))
            }
            AssignTarget::DestructuringObject(props) => {
                let p: Vec<String> = props.iter()
                    .map(|(k, v, def)| {
                        let target_str = self.generate_assign_target(v);
                        let base = if let AssignTarget::Variable(name) = v {
                            if name == k {
                                k.clone()
                            } else {
                                format!("{k}: {target_str}")
                            }
                        } else {
                            format!("{k}: {target_str}")
                        };

                        if let Some(d) = def {
                            format!("{} = {}", base, self.generate_expr(d))
                        } else {
                            base
                        }
                    })
                    .collect();
                format!("{{ {} }}", p.join(", "))
            }
            AssignTarget::DestructuringObjectRest { properties, rest } => {
                let mut p: Vec<String> = properties.iter()
                    .map(|(k, v, def)| {
                        let target_str = self.generate_assign_target(v);
                        let base = if let AssignTarget::Variable(name) = v {
                            if name == k {
                                k.clone()
                            } else {
                                format!("{k}: {target_str}")
                            }
                        } else {
                            format!("{k}: {target_str}")
                        };

                        if let Some(d) = def {
                            format!("{} = {}", base, self.generate_expr(d))
                        } else {
                            base
                        }
                    })
                    .collect();
                p.push(format!("...{}", self.generate_assign_target(rest)));
                format!("{{ {} }}", p.join(", "))
            }
            AssignTarget::Rest(inner) => format!("...{}", self.generate_assign_target(inner)),
        }
    }
}
