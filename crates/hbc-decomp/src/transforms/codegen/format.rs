use super::Codegen;

impl Codegen {
    pub(super) fn format_member_access(
        &self,
        obj: &str,
        opt: &str,
        key: &crate::ir::PropertyKey,
    ) -> String {
        crate::ir::expr::display::format_member_access_with(obj, opt, key, |e| {
            self.generate_expr(e)
        })
    }

    pub(super) fn format_call(
        &self,
        callee_str: &str,
        _callee_expr: Option<&crate::ir::Expression>,
        arguments: &[crate::ir::Expression],
        extra_suffix: &str,
    ) -> String {
        // After strip_hermes_this() in the pipeline, `this` has already been removed from
        // Call arguments. All remaining arguments are real user-visible arguments.
        format!(
            "{}({}){}",
            callee_str,
            self.join_exprs(arguments),
            extra_suffix
        )
    }

    pub(super) fn format_property(&self, prop: &crate::ir::ObjectProperty) -> String {
        use crate::ir::{Expression, PropertyKey, Value};

        // Shorthand: { x } instead of { x: x }
        if let PropertyKey::Ident(key_name) = &prop.key {
            if let Expression::Value(Value::Binding(crate::ir::Binding::Variable(var_name))) =
                &prop.value
            {
                // Only a key that is itself an identifier can be shorthand;
                // `{ "a.b" }` and `{ [[Value]] }` are not.
                if key_name == var_name
                    && crate::util::is_valid_identifier(key_name)
                    && !crate::constants::is_reserved_word(key_name)
                {
                    return key_name.clone();
                }
            }
        }

        // Method shorthand: { foo() { ... } } instead of { foo: function foo() { ... } }
        if let PropertyKey::Ident(key_name) = &prop.key {
            if let Expression::Function {
                name: Some(fn_name),
                is_generator,
                is_async,
                ..
            } = &prop.value
            {
                // `parse scheme start() {}` is not a method; a key that is not
                // an identifier keeps the `key: function` form, quoted below.
                if key_name == fn_name && crate::util::is_valid_identifier(key_name) {
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
                        // The inline body came out as an arrow or something
                        // else that is not a declaration: no method shorthand,
                        // and the key must not be lost.
                        return format!(
                            "{}: {rendered}",
                            crate::ir::expr::display::format_key(&prop.key)
                        );
                    };
                    return stripped;
                }
            }
        }

        format!(
            "{}: {}",
            crate::ir::expr::display::format_key(&prop.key),
            self.generate_expr(&prop.value)
        )
    }

    pub(super) fn generate_assign_target(&self, target: &crate::ir::AssignTarget) -> String {
        use crate::ir::AssignTarget;
        match target {
            AssignTarget::Binding(crate::ir::Binding::Register(r)) => format!("r{r}"),
            AssignTarget::Binding(crate::ir::Binding::Variable(n)) => {
                crate::util::sanitize_identifier(n)
            }
            AssignTarget::Member { object, property } => {
                let obj = self.generate_expr(object);
                let obj = if matches!(
                    object,
                    crate::ir::Expression::Binary { .. }
                        | crate::ir::Expression::Conditional { .. }
                        | crate::ir::Expression::Assignment { .. }
                ) {
                    format!("({obj})")
                } else {
                    obj
                };
                // Same rules as Expression::Member, non-identifier keys need brackets.
                crate::ir::expr::display::format_member_access_with(
                    &obj,
                    "",
                    &crate::ir::PropertyKey::Ident(property.clone()),
                    |e| self.generate_expr(e),
                )
            }
            AssignTarget::Index { object, key } => {
                let obj = self.generate_expr(object);
                let k = self.generate_expr(key);
                format!("{obj}[{k}]")
            }
            // Must match `Value::closure_var_name` so load/store of the same
            // captured slot use the same identifier.
            AssignTarget::Binding(crate::ir::Binding::ClosureVar { level, slot }) => {
                crate::ir::Value::closure_var_name(*level, *slot)
            }
            AssignTarget::DestructuringArray(elements) => {
                let elems: Vec<String> = elements
                    .iter()
                    .map(|e| {
                        e.as_ref()
                            .map(|(t, def)| {
                                let t_str = self.generate_assign_target(t);
                                if let Some(d) = def {
                                    format!("{} = {}", t_str, self.generate_expr(d))
                                } else {
                                    t_str
                                }
                            })
                            .unwrap_or_default()
                    })
                    .collect();
                format!("[{}]", elems.join(", "))
            }
            AssignTarget::DestructuringArrayRest { elements, rest } => {
                let mut elems: Vec<String> = elements
                    .iter()
                    .map(|e| {
                        e.as_ref()
                            .map(|(t, def)| {
                                let t_str = self.generate_assign_target(t);
                                if let Some(d) = def {
                                    format!("{} = {}", t_str, self.generate_expr(d))
                                } else {
                                    t_str
                                }
                            })
                            .unwrap_or_default()
                    })
                    .collect();
                elems.push(format!("...{}", self.generate_assign_target(rest)));
                format!("[{}]", elems.join(", "))
            }
            AssignTarget::DestructuringObject(props) => {
                let p: Vec<String> = props
                    .iter()
                    .map(|(k, v, def)| {
                        let target_str = self.generate_assign_target(v);
                        let key = destructuring_key(k);
                        let base =
                            if let AssignTarget::Binding(crate::ir::Binding::Variable(name)) = v {
                                if name == k && key == *k {
                                    k.clone()
                                } else {
                                    format!("{key}: {target_str}")
                                }
                            } else {
                                format!("{key}: {target_str}")
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
                let mut p: Vec<String> = properties
                    .iter()
                    .map(|(k, v, def)| {
                        let target_str = self.generate_assign_target(v);
                        let key = destructuring_key(k);
                        let base =
                            if let AssignTarget::Binding(crate::ir::Binding::Variable(name)) = v {
                                if name == k && key == *k {
                                    k.clone()
                                } else {
                                    format!("{key}: {target_str}")
                                }
                            } else {
                                format!("{key}: {target_str}")
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

// A destructuring key that is not an identifier (`aria-busy`) has to be
// quoted, as in an object literal.
fn destructuring_key(key: &str) -> String {
    if crate::util::is_valid_identifier(key) {
        key.to_string()
    } else {
        crate::util::escape_js_string(key)
    }
}
