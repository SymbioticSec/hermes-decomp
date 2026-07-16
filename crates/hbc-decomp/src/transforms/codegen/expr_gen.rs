use super::{Codegen, indent_multiline};

impl Codegen {
    // Generate code for an expression. Handles all types recursively so that
    // inline function bodies and import comments are applied at any nesting depth.
    pub(super) fn generate_expr(&self, expr: &crate::ir::Expression) -> String {
        use crate::ir::{Expression, Value, Constant};

        match expr {
            Expression::Value(v) => format!("{v}"),
            Expression::Binary { op, left, right } => {
                let prec = op.precedence();
                // Exponentiation (**) is right-associative: left needs higher threshold
                // All other operators are left-associative: right needs higher threshold
                let (l_prec, r_prec) = if matches!(op, crate::ir::BinaryOp::Exp) {
                    (prec + 1, prec) // right-assoc: parens on left if same prec
                } else {
                    (prec, prec + 1) // left-assoc: parens on right if same prec
                };
                let l = self.generate_expr_with_parens(left, l_prec);
                let r = self.generate_expr_with_parens(right, r_prec);
                format!("{l} {op} {r}")
            }
            Expression::Unary { op, operand } => {
                // Optimize !comparison → negated comparison (e.g., !(x < y) → x >= y)
                if matches!(op, crate::ir::UnaryOp::Not) {
                    if let Expression::Binary { op: bin_op, left, right } = operand.as_ref() {
                        let negated = match bin_op {
                            crate::ir::BinaryOp::Eq => Some(crate::ir::BinaryOp::Neq),
                            crate::ir::BinaryOp::Neq => Some(crate::ir::BinaryOp::Eq),
                            crate::ir::BinaryOp::StrictEq => Some(crate::ir::BinaryOp::StrictNeq),
                            crate::ir::BinaryOp::StrictNeq => Some(crate::ir::BinaryOp::StrictEq),
                            crate::ir::BinaryOp::Lt => Some(crate::ir::BinaryOp::Ge),
                            crate::ir::BinaryOp::Le => Some(crate::ir::BinaryOp::Gt),
                            crate::ir::BinaryOp::Gt => Some(crate::ir::BinaryOp::Le),
                            crate::ir::BinaryOp::Ge => Some(crate::ir::BinaryOp::Lt),
                            _ => None,
                        };
                        if let Some(neg_op) = negated {
                            let prec = neg_op.precedence();
                            let l = self.generate_expr_with_parens(left, prec);
                            let r = self.generate_expr_with_parens(right, prec);
                            return format!("{l} {neg_op} {r}");
                        }
                        // Non-comparison binary: add parens to avoid precedence issues
                        return format!("{op}({})", self.generate_expr(operand));
                    }
                }
                // For unary applied to non-binary expressions, check if parens needed
                match operand.as_ref() {
                    Expression::Conditional { .. } | Expression::Assignment { .. } => {
                        format!("{op}({})", self.generate_expr(operand))
                    }
                    _ => format!("{op}{}", self.generate_expr(operand)),
                }
            }
            Expression::Conditional { condition, then_expr, else_expr } => {
                format!("{} ? {} : {}",
                    self.generate_expr(condition),
                    self.generate_expr(then_expr),
                    self.generate_expr(else_expr)
                )
            }
            Expression::Member { object, property, optional } => {
                // Simplify _interopDefault(X).default → X, _interopRequireDefault(X).default → X
                // The interop wrapper + .default access cancel out
                if !*optional {
                    if let crate::ir::PropertyKey::Ident(prop) = property {
                        if prop == "default" {
                            if let Expression::Call { callee: interop_callee, arguments: interop_args } = object.as_ref() {
                                if interop_args.len() == 1 {
                                    let is_interop = match interop_callee.as_ref() {
                                        Expression::Value(Value::Variable(n)) => {
                                            n.contains("interop") || n == "_interopDefault"
                                                || n == "_interopRequireDefault" || n == "_interopNamespace"
                                        }
                                        _ => false,
                                    };
                                    if is_interop {
                                        return self.generate_expr(&interop_args[0]);
                                    }
                                }
                            }
                        }
                    }
                }
                // Simplify globalThis.X → X for well-known built-in globals
                if !*optional {
                    if let crate::ir::PropertyKey::Ident(name) = property {
                        let is_global = match &**object {
                            Expression::Value(Value::Global) => true,
                            Expression::Value(Value::Variable(v)) if v == "globalThis" => true,
                            _ => false,
                        };
                        if is_global && crate::ir::expr::display::is_builtin_global(name) {
                            return name.clone();
                        }
                    }
                }
                let obj = self.generate_expr(object);
                let opt = if *optional { "?" } else { "" };
                // Post-render simplification: if the object rendered to "globalThis"
                // (e.g. from nested Member like scope.globalThis), also simplify builtins
                if opt.is_empty() {
                    if let crate::ir::PropertyKey::Ident(name) = property {
                        if obj == "globalThis" && crate::ir::expr::display::is_builtin_global(name) {
                            return name.clone();
                        }
                    }
                }
                self.format_member_access(&obj, opt, property)
            }
            Expression::Call { callee, arguments } => {
                // In ESM mode, try to resolve require calls to readable require("moduleName")
                if self.esm_mode {
                    if let Some(mod_name) = self.resolve_require_module(expr) {
                        return format!("require(\"{mod_name}\")");
                    }
                }

                // Simplify X.apply(this, arguments) → X(...arguments)
                // This is the Babel _asyncToGenerator wrapper pattern
                // Hermes args can be either:
                //   3 args: [X (method this), this_value, arguments_array]
                //   2 args: [this_value, arguments_array]
                if let Expression::Member { object, property: crate::ir::PropertyKey::Ident(method), .. } = callee.as_ref() {
                    if method == "apply" && arguments.len() >= 2 {
                        let args_obj = &arguments[arguments.len() - 1];
                        let args_str = self.generate_expr(args_obj);
                        if args_str == "arguments" {
                            let obj_str = self.generate_expr(object);
                            return format!("{obj_str}(...arguments)");
                        }
                    }
                }

                let mut comment = String::new();
                let callee_str = self.generate_expr(callee);

                if callee_str == "require" {
                    if let Some(map) = &self.import_map {
                        let id_arg = if arguments.len() >= 2 {
                            if matches!(&arguments[0], Expression::Value(Value::Constant(Constant::Undefined))) {
                                arguments.get(1)
                            } else {
                                arguments.first()
                            }
                        } else {
                            arguments.first()
                        };
                        if let Some(Expression::Value(Value::Constant(Constant::Integer(id)))) = id_arg {
                            if let Some(name) = map.get(&(*id as u32)) {
                                comment = format!(" /* {name} */");
                            }
                        }
                    }
                }

                // Simplify HermesInternal.concat.call(a, b, c, ...) → a + b + c + ...
                // Also handles X.HermesInternal.concat.call(...) and HermesInternal.concat(...)
                if callee_str.ends_with("HermesInternal.concat.call")
                    || callee_str.ends_with("HermesInternal.concat")
                    || callee_str.ends_with("HermesInternal.HermesInternal.concat.call")
                    || callee_str.ends_with("HermesInternal.HermesInternal.concat")
                {
                    let is_call = callee_str.ends_with(".call");
                    // .call(thisArg, arg1, arg2, ...) → thisArg + arg1 + arg2 + ...
                    // direct(arg1, arg2, ...) → arg1 + arg2 + ...
                    let parts: Vec<String> = arguments.iter()
                        .map(|a| self.generate_expr(a))
                        .collect();
                    if parts.is_empty() {
                        return "\"\"".to_string();
                    }
                    // For .call(), first arg is `this` (typically "" or ``).
                    // Skip it if it's an empty string/template.
                    let start = if is_call {
                        let first = &parts[0];
                        if first == "\"\"" || first == "''" || first == "``" || first == "`\\``" {
                            1 // skip empty separator
                        } else {
                            0 // non-empty prefix, include it
                        }
                    } else {
                        0
                    };
                    if start >= parts.len() {
                        return "\"\"".to_string();
                    }
                    return parts[start..].join(" + ");
                }

                // Transform HermesBuiltin_* closure calls to clean JS equivalents
                // These are compiler-generated helper functions in the bytecode string table
                // Pattern 1: HermesBuiltin_X.call(thisArg, args...) → clean_name(args...)
                // Pattern 2: HermesBuiltin_X(args...) → clean_name(args...)
                if callee_str.contains("HermesBuiltin_") {
                    let is_dot_call = callee_str.ends_with(".call");
                    // Extract the builtin name (e.g., "getTemplateObject" from "HermesBuiltin_getTemplateObject.call")
                    let base = if is_dot_call {
                        callee_str.trim_end_matches(".call")
                    } else {
                        &callee_str
                    };
                    // Find the HermesBuiltin_ part and extract the suffix
                    if let Some(pos) = base.find("HermesBuiltin_") {
                        let builtin_name = &base[pos + "HermesBuiltin_".len()..];

                        // For .call(), skip first arg (thisArg)
                        let real_args: Vec<String> = if is_dot_call && arguments.len() > 1 {
                            arguments[1..].iter().map(|a| self.generate_expr(a)).collect()
                        } else if is_dot_call {
                            vec![]
                        } else {
                            arguments.iter().map(|a| self.generate_expr(a)).collect()
                        };

                        // Map to clean JS equivalents
                        let clean_call = match builtin_name {
                            "copyDataProperties" => {
                                // copyDataProperties(target, source, excludes) → Object.assign(target, source)
                                format!("Object.assign({})", real_args.join(", "))
                            }
                            _ => {
                                // Strip prefix, keep as function call
                                format!("{}({})", builtin_name, real_args.join(", "))
                            }
                        };
                        return clean_call;
                    }
                }

                self.format_call(&callee_str, Some(callee.as_ref()), arguments, &comment)
            }
            Expression::New { callee, arguments } => {
                format!("new {}({})", self.generate_expr(callee), self.join_exprs(arguments))
            }
            Expression::Array { elements } => {
                let elems: Vec<String> = elements.iter()
                    .map(|e| e.as_ref().map(|x| self.generate_expr(x)).unwrap_or_default())
                    .collect();
                let has_multiline = elems.iter().any(|e| e.contains('\n'));
                if has_multiline {
                    let indent = self.current_indent();
                    let inner_indent = format!("{indent}  ");
                    let items = elems.iter()
                        .map(|e| indent_multiline(e, &inner_indent))
                        .collect::<Vec<_>>()
                        .join(",\n");
                    format!("[\n{items}\n{indent}]")
                } else {
                    format!("[{}]", elems.join(", "))
                }
            }
            Expression::Object { properties } => {
                if properties.is_empty() {
                    "{}".to_string()
                } else {
                    let props: Vec<String> = properties.iter().map(|p| self.format_property(p)).collect();
                    let has_multiline = props.iter().any(|p| p.contains('\n'));
                    if has_multiline {
                        let indent = self.current_indent();
                        let inner_indent = format!("{indent}  ");
                        let items = props.iter()
                            .map(|p| indent_multiline(p, &inner_indent))
                            .collect::<Vec<_>>()
                            .join(",\n");
                        format!("{{\n{items}\n{indent}}}")
                    } else {
                        format!("{{ {} }}", props.join(", "))
                    }
                }
            }
            Expression::Function { id, name, is_arrow, is_async, is_generator } => {
                if let Some(rendered) = self.inline_bodies.get(&id.0) {
                    // Re-indent based on current context: first line stays, rest get current indent
                    if self.indent_level > 0 {
                        let indent = self.current_indent();
                        let mut lines = rendered.lines();
                        let mut result = String::new();
                        if let Some(first) = lines.next() {
                            result.push_str(first);
                        }
                        for line in lines {
                            result.push('\n');
                            if !line.is_empty() {
                                result.push_str(&indent);
                            }
                            result.push_str(line);
                        }
                        result
                    } else {
                        rendered.clone()
                    }
                } else {
                    let async_prefix = if *is_async { "async " } else { "" };
                    // Async generators (Babel pattern) render as async, not function*
                    let gen_star = if *is_generator && !*is_async { "*" } else { "" };
                    match (is_arrow, name) {
                        (true, Some(n)) => format!("{async_prefix}function {n}() {{ ... }}"),
                        (true, None) => format!("{async_prefix}() => {{ ... }}"),
                        (false, Some(n)) => format!("{async_prefix}function{gen_star} {n}() {{ ... }}"),
                        (false, None) => format!("/* F{} */ {}function{}() {{ ... }}", id.0, async_prefix, gen_star),
                    }
                }
            }
            Expression::Assignment { target, value } => {
                format!("{} = {}", self.generate_expr(target), self.generate_expr(value))
            }
            Expression::Spread(inner) => format!("...{}", self.generate_expr(inner)),
            Expression::TemplateLiteral { quasis, expressions } => {
                let mut out = String::from("`");
                for (i, quasi) in quasis.iter().enumerate() {
                    out.push_str(quasi);
                    if let Some(e) = expressions.get(i) {
                        out.push_str(&format!("${{{}}}", self.generate_expr(e)));
                    }
                }
                out.push('`');
                out
            }
            Expression::RegExp { pattern, flags } => format!("/{pattern}/{flags}"),
            Expression::Yield { value, delegate } => {
                if *delegate {
                    format!("yield* {}", self.generate_expr(value))
                } else {
                    format!("yield {}", self.generate_expr(value))
                }
            }
            Expression::Await(value) => format!("await {}", self.generate_expr(value)),
            Expression::JSXElement { tag, attributes, children } => {
                use crate::ir::{Constant, Value};
                let mut attrs = Vec::new();
                for (key, val) in attributes {
                    if key == "..." {
                        // Spread props: `{...obj}`.
                        attrs.push(format!("{{...{}}}", self.generate_expr(val)));
                    } else if let Expression::Value(Value::Constant(Constant::String(s))) = val {
                        // String value → `name="..."` (idiomatic JSX, not `={"..."}`).
                        attrs.push(format!("{key}={s:?}"));
                    } else if matches!(val, Expression::Value(Value::Constant(Constant::Bool(true)))) {
                        // `name={true}` → shorthand bare `name`.
                        attrs.push(key.clone());
                    } else {
                        attrs.push(format!("{}={{{}}}", key, self.generate_expr(val)));
                    }
                }
                // A Fragment carries an empty tag → `<>...</>`.
                let (open, close) = if tag.is_empty() {
                    (String::new(), String::new())
                } else {
                    (tag.clone(), tag.clone())
                };
                let attr_str = if attrs.is_empty() { String::new() } else { format!(" {}", attrs.join(" ")) };
                if children.is_empty() {
                    if tag.is_empty() {
                        return "<></>".to_string();
                    }
                    format!("<{open}{attr_str} />")
                } else {
                    // A nested element renders inline; any other expression child
                    // (variable, call, string, ...) must be wrapped in `{ }`.
                    let child_str: Vec<String> = children
                        .iter()
                        .map(|c| match c {
                            crate::ir::Expression::JSXElement { .. } => self.generate_expr(c),
                            _ => format!("{{{}}}", self.generate_expr(c)),
                        })
                        .collect();
                    format!("<{open}{attr_str}>{}</{close}>", child_str.join(""))
                }
            }
            Expression::Unknown { opcode, operands } => format!("/* {} {} */", opcode, operands.join(", ")),
        }
    }

    pub(super) fn generate_expr_with_parens(&self, expr: &crate::ir::Expression, parent_prec: u8) -> String {
        let needs_parens = match expr {
            crate::ir::Expression::Binary { op, .. } => op.precedence() < parent_prec,
            _ => false,
        };
        let s = self.generate_expr(expr);
        if needs_parens { format!("({s})") } else { s }
    }

    pub(super) fn join_exprs(&self, exprs: &[crate::ir::Expression]) -> String {
        exprs.iter().map(|e| self.generate_expr(e)).collect::<Vec<_>>().join(", ")
    }
}
