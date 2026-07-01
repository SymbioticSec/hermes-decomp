use super::{Codegen, sanitize_loop_var, replace_whole_word};
use crate::ir::Statement;

impl Codegen {
    pub(super) fn generate_stmt(&mut self, stmt: &Statement) -> String {
        let indent = self.current_indent();
        match stmt {
            Statement::Expr(e) => format!("{indent}{};\n", self.generate_expr(e)),
            Statement::Let { name, value, kind } => {
                // Skip invalid JS identifiers (numbers, string literals)
                let first_char = name.chars().next().unwrap_or('_');
                if first_char.is_ascii_digit() || first_char == '"' || first_char == '\'' {
                    return String::new();
                }
                // Skip global aliases (const Object = globalThis.Object, const Object2 = ...)
                if Self::is_global_alias_def(name, value) {
                    return String::new();
                }
                // Skip self-assignments (const x = x)
                if let crate::ir::Expression::Value(crate::ir::Value::Variable(v)) = value {
                    if v == name {
                        return String::new();
                    }
                }
                // Simplify `let x = undefined;` → `let x;` and `const x = undefined;` → `let x;`
                if matches!(value, crate::ir::Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::Undefined))) {
                    return format!("{indent}let {name};\n");
                }
                // Convert `const name = function name(...)` → `function name(...)`
                // when function name matches variable name (redundant named expression)
                if let crate::ir::Expression::Function { name: Some(fn_name), .. } = value {
                    if fn_name == name {
                        let rendered = self.generate_expr(value);
                        return format!("{indent}{rendered}\n");
                    }
                }
                format!("{indent}{kind} {name} = {};\n", self.generate_expr(value))
            }
            Statement::Assign { target, value } => {
                // Destructuring targets. The bound names are declared once at the
                // function top (hoisted by insert_declarations) so two patterns
                // that share a register-derived name don't each emit a clashing
                // `let`. Emit the pattern as a bare assignment; object patterns
                // are parenthesized so `{` is not parsed as a block.
                if matches!(target,
                    crate::ir::AssignTarget::DestructuringObject(_)
                    | crate::ir::AssignTarget::DestructuringObjectRest { .. }
                ) {
                    return format!("{indent}({} = {});\n", self.generate_assign_target(target), self.generate_expr(value));
                }
                if matches!(target,
                    crate::ir::AssignTarget::DestructuringArray(_)
                    | crate::ir::AssignTarget::DestructuringArrayRest { .. }
                ) {
                    return format!("{indent}{} = {};\n", self.generate_assign_target(target), self.generate_expr(value));
                }
                // Skip assigns to invalid variable names (numeric constants like `0 = 0;`)
                if let crate::ir::AssignTarget::Variable(name) = target {
                    let first_char = name.chars().next().unwrap_or('_');
                    if first_char.is_ascii_digit() || first_char == '"' || first_char == '\'' {
                        return String::new();
                    }
                    // Also skip self-assignments: `x = x;`
                    if let crate::ir::Expression::Value(crate::ir::Value::Variable(v)) = value {
                        if v == name {
                            return String::new();
                        }
                    }
                }
                // Skip module.exports.__esModule and module.exports.default = module.exports
                if let crate::ir::AssignTarget::Member { object, property } = target {
                    let obj_str = self.generate_expr(object);
                    if obj_str.ends_with(".exports") {
                        if property == "__esModule" {
                            return String::new();
                        }
                        if property == "default" {
                            let val_str = self.generate_expr(value);
                            if val_str.ends_with(".exports") || val_str == "module.exports" {
                                return String::new();
                            }
                        }
                    }
                }
                format!("{indent}{} = {};\n", self.generate_assign_target(target), self.generate_expr(value))
            }
            Statement::Delete { target, result: _ } => {
                let target_str = self.generate_expr(target);
                format!("{indent}delete {target_str};\n")
            }
            Statement::Break(label) => {
                if let Some(l) = label {
                    format!("{indent}break {l};\n")
                } else {
                    format!("{indent}break;\n")
                }
            }
            Statement::Continue(label) => {
                if let Some(l) = label {
                    format!("{indent}continue {l};\n")
                } else {
                    format!("{indent}continue;\n")
                }
            }
            Statement::Return(Some(e)) => {
                // return undefined; → return;
                if matches!(e, crate::ir::Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::Undefined))) {
                    format!("{indent}return;\n")
                } else {
                    format!("{indent}return {};\n", self.generate_expr(e))
                }
            }
            Statement::Return(None) => format!("{indent}return;\n"),
            Statement::Throw(e) => format!("{indent}throw {};\n", self.generate_expr(e)),
            Statement::Debugger => format!("{indent}debugger;\n"),
            Statement::Comment(s) => {
                // Skip label debug comments unless explicitly requested
                if !self.options.include_labels && s.ends_with(':') && s.starts_with("label") {
                    return String::new();
                }
                format!("{indent}// {s}\n")
            }
            Statement::Goto(t) => format!("{indent}goto {t};\n"),
            Statement::CondGoto {
                condition,
                target,
                fallthrough,
            } => {
                format!("{indent}if ({}) goto {target} else goto {fallthrough};\n", self.generate_expr(condition))
            }
            Statement::If {
                condition,
                then_body,
                else_body,
            } => self.generate_if(condition, then_body, else_body),
            Statement::While { condition, body } => self.generate_while(condition, body),
            Statement::DoWhile { body, condition } => self.generate_do_while(body, condition),
            Statement::For {
                init,
                condition,
                update,
                body,
            } => self.generate_for(init.as_deref(), condition.as_ref(), update.as_deref(), body),
            Statement::ForOf {
                variable,
                iterable,
                body,
            } => {
                let var_name = sanitize_loop_var(variable, "item");
                let mut out = format!("{indent}for (const {var_name} of {}) {{\n", self.generate_expr(iterable));
                self.indent_level += 1;
                let body_str = self.generate_statements(body);
                if var_name != *variable {
                    out.push_str(&replace_whole_word(&body_str, variable, &var_name));
                } else {
                    out.push_str(&body_str);
                }
                self.indent_level -= 1;
                out.push_str(&format!("{indent}}}\n"));
                out
            }
            Statement::ForIn {
                variable,
                object,
                body,
            } => {
                let var_name = sanitize_loop_var(variable, "key");
                let mut out = format!("{indent}for (const {var_name} in {}) {{\n", self.generate_expr(object));
                self.indent_level += 1;
                let body_str = self.generate_statements(body);
                if var_name != *variable {
                    out.push_str(&replace_whole_word(&body_str, variable, &var_name));
                } else {
                    out.push_str(&body_str);
                }
                self.indent_level -= 1;
                out.push_str(&format!("{indent}}}\n"));
                out
            }
            Statement::Switch {
                discriminant,
                cases,
                default,
            } => {
                let mut out = format!("{indent}switch ({}) {{\n", self.generate_expr(discriminant));
                self.indent_level += 1;
                let case_indent = self.current_indent();

                for (val, body) in cases {
                    out.push_str(&format!("{case_indent}case {}:\n", self.generate_expr(val)));
                    self.indent_level += 1;
                    out.push_str(&self.generate_statements(body));
                    self.indent_level -= 1;
                    // Auto-insert break if needed? For now we assume body flows correctly or we accept fallthrough
                    // But in reconstruction we usually want breaks.
                    // If the body doesn't end in return/break/throw/continue, we might want to add break?
                    // Let's check last statement.
                    if let Some(last) = body.last() {
                        match last {
                            Statement::Return(_)
                            | Statement::Throw(_)
                            | Statement::Goto(_)
                            | Statement::CondGoto { .. } => {}
                            Statement::Comment(c) if c == "break" || c == "continue" => {}
                            _ => {
                                // Add break
                                out.push_str(&format!("{}break;\n", self.current_indent()));
                            }
                        }
                    } else {
                        // Empty body needs break
                        out.push_str(&format!("{}break;\n", self.current_indent()));
                    }
                }

                if let Some(default_body) = default {
                    out.push_str(&format!("{case_indent}default:\n"));
                    self.indent_level += 1;
                    out.push_str(&self.generate_statements(default_body));
                    self.indent_level -= 1;
                }

                self.indent_level -= 1;
                out.push_str(&format!("{indent}}}\n"));
                out
            }
            Statement::TryCatch {
                try_body,
                catch_param,
                catch_body,
                finally_body,
            } => {
                self.generate_try_catch(try_body, catch_param.as_deref(), catch_body, finally_body)
            }
            Statement::Block(stmts) => {
                if stmts.is_empty() {
                    return String::new(); // Skip empty blocks (produced by optimize pass)
                }
                let mut out = format!("{indent}{{\n");
                self.indent_level += 1;
                out.push_str(&self.generate_statements(stmts));
                self.indent_level -= 1;
                out.push_str(&format!("{}}}\n", self.current_indent()));
                out
            }
            Statement::Class {
                name,
                super_class,
                methods,
                ..
            } => {
                let mut out = format!("{indent}class {name}");
                if let Some(sc) = super_class {
                    out.push_str(&format!(" extends {}", self.generate_expr(sc)));
                }
                out.push_str(" {\n");

                self.indent_level += 1;
                // Generate methods
                for method in methods {
                    let method_indent = self.current_indent();
                    if method.is_static {
                        out.push_str(&format!("{method_indent}static "));
                    } else {
                        out.push_str(&method_indent);
                    }

                    // Handle method kind (getter/setter)
                    let kind_prefix = match method.kind {
                        crate::ir::MethodKind::Getter => "get ",
                        crate::ir::MethodKind::Setter => "set ",
                        _ => "",
                    };

                    if let crate::ir::Expression::Function {
                        is_async,
                        is_generator,
                        ..
                    } = &method.value
                    {
                        let async_prefix = if *is_async { "async " } else { "" };
                        // Async generators (Babel pattern) render as async, not function*
                        let gen = if *is_generator && !*is_async { "*" } else { "" };
                        let params = method.params.join(", ");

                        if let Some(body) = &method.body {
                            out.push_str(&format!(
                                "{kind_prefix}{async_prefix}{gen}{}({params}) {{\n",
                                method.key
                            ));
                            self.indent_level += 1;
                            out.push_str(&self.generate_statements(body));
                            self.indent_level -= 1;
                            out.push_str(&format!("{method_indent}}}\n"));
                        } else {
                            out.push_str(&format!(
                                "{kind_prefix}{async_prefix}{gen}{}({params}) {{ /* compiled code */ }}\n",
                                method.key
                            ));
                        }
                    } else {
                        // Fallback
                        out.push_str(&format!("{kind_prefix}{}() {{ ... }}\n", method.key));
                    }
                }

                self.indent_level -= 1;
                out.push_str(&format!("{indent}}}\n"));
                out
            }
        }
    }
}
