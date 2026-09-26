use super::{replace_whole_word, sanitize_loop_var, Codegen};
use crate::ir::{Binding, Statement};

impl Codegen {
    // Each case is its own block. A `let` in one case must not be in scope
    // for the next, and must not be in the temporal dead zone when that
    // other case runs.
    fn case_block(&mut self, body: &[Statement]) -> String {
        let case_indent = self.current_indent();
        self.indent_level += 1;
        let mut out = format!("{case_indent}{{\n");
        out.push_str(&self.generate_statements(body));
        let needs_break = match body.last() {
            Some(
                Statement::Return(_)
                | Statement::Throw(_)
                | Statement::Goto(_)
                | Statement::CondGoto { .. },
            ) => false,
            Some(Statement::Comment(c)) if c == "break" || c == "continue" => false,
            _ => true,
        };
        if needs_break {
            out.push_str(&format!("{}break;\n", self.current_indent()));
        }
        self.indent_level -= 1;
        out.push_str(&format!("{case_indent}}}\n"));
        out
    }

    pub(super) fn generate_stmt(&mut self, stmt: &Statement) -> String {
        let indent = self.current_indent();
        match stmt {
            Statement::Expr(e) => {
                let rendered = self.generate_expr(e);
                // A statement that starts with `{` parses as a block and one
                // that starts with `function` as a declaration; both need the
                // parentheses to read as the expression they are.
                if rendered.starts_with('{')
                    || rendered.starts_with("function")
                    || rendered.starts_with("async function")
                {
                    format!("{indent}({rendered});\n")
                } else {
                    format!("{indent}{rendered};\n")
                }
            }
            Statement::Let { name, value, kind } => {
                let name = crate::util::sanitize_identifier(name);
                // Skip invalid JS identifiers (numbers, string literals)
                let first_char = name.chars().next().unwrap_or('_');
                if first_char.is_ascii_digit() || first_char == '"' || first_char == '\'' {
                    return String::new();
                }
                // Skip global aliases (const Object = globalThis.Object, const Object2 = ...)
                if Self::is_global_alias_def(&name, value) {
                    return String::new();
                }
                // Skip self-assignments (const x = x)
                if let crate::ir::Expression::Value(crate::ir::Value::Binding(Binding::Variable(
                    v,
                ))) = value
                {
                    if crate::util::sanitize_identifier(v) == name {
                        return String::new();
                    }
                }
                // Simplify `let x = undefined;` → `let x;` and `const x = undefined;` → `let x;`
                if matches!(
                    value,
                    crate::ir::Expression::Value(crate::ir::Value::Constant(
                        crate::ir::Constant::Undefined
                    ))
                ) {
                    return format!("{indent}let {name};\n");
                }
                // Convert `const name = function name(...)` → `function name(...)`
                // when function name matches variable name (redundant named expression)
                if let crate::ir::Expression::Function {
                    name: Some(fn_name),
                    ..
                } = value
                {
                    if crate::util::sanitize_identifier(fn_name) == name {
                        let rendered = self.generate_expr(value);
                        // The inline body decides the final shape; an arrow
                        // is not a declaration and keeps its `const`.
                        // Only a declaration that carries the name may stand
                        // alone; an anonymous `function(...)` or an arrow
                        // keeps its `const`.
                        let declares = [
                            format!("function {name}("),
                            format!("function* {name}("),
                            format!("async function {name}("),
                            format!("async function* {name}("),
                        ];
                        if declares.iter().any(|d| rendered.starts_with(d.as_str())) {
                            return format!("{indent}{rendered}\n");
                        }
                        return format!("{indent}{kind} {name} = {rendered};\n");
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
                if matches!(
                    target,
                    crate::ir::AssignTarget::DestructuringObject(_)
                        | crate::ir::AssignTarget::DestructuringObjectRest { .. }
                ) {
                    return format!(
                        "{indent}({} = {});\n",
                        self.generate_assign_target(target),
                        self.generate_expr(value)
                    );
                }
                if matches!(
                    target,
                    crate::ir::AssignTarget::DestructuringArray(_)
                        | crate::ir::AssignTarget::DestructuringArrayRest { .. }
                ) {
                    return format!(
                        "{indent}{} = {};\n",
                        self.generate_assign_target(target),
                        self.generate_expr(value)
                    );
                }
                // Skip assigns to invalid variable names (numeric constants like `0 = 0;`)
                if let crate::ir::AssignTarget::Binding(Binding::Variable(name)) = target {
                    let first_char = name.chars().next().unwrap_or('_');
                    if first_char.is_ascii_digit() || first_char == '"' || first_char == '\'' {
                        return String::new();
                    }
                    // Also skip self-assignments: `x = x;`
                    if let crate::ir::Expression::Value(crate::ir::Value::Binding(
                        Binding::Variable(v),
                    )) = value
                    {
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
                let target_str = self.generate_assign_target(target);
                // `{ a: 1 }.b = v` parses as a block; a parenthesised member
                // expression is still a valid assignment target.
                let target_str = if target_str.starts_with('{')
                    || target_str.starts_with("function")
                    || target_str.starts_with("class ")
                {
                    format!("({target_str})")
                } else {
                    target_str
                };
                format!("{indent}{target_str} = {};\n", self.generate_expr(value))
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
                if matches!(
                    e,
                    crate::ir::Expression::Value(crate::ir::Value::Constant(
                        crate::ir::Constant::Undefined
                    ))
                ) {
                    format!("{indent}return;\n")
                } else {
                    format!("{indent}return {};\n", self.generate_expr(e))
                }
            }
            Statement::Return(None) => format!("{indent}return;\n"),
            Statement::Throw(e) => format!("{indent}throw {};\n", self.generate_expr(e)),
            Statement::Debugger => format!("{indent}debugger;\n"),
            Statement::Comment(s) => {
                // Loop labels are placed by `generate_statements`, on the loop
                // they belong to; one that reaches here has no loop.
                if super::loop_label(s).is_some() {
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
                format!(
                    "{indent}if ({}) goto {target} else goto {fallthrough};\n",
                    self.generate_expr(condition)
                )
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
                let mut out = format!(
                    "{indent}for (const {var_name} of {}) {{\n",
                    self.generate_expr(iterable)
                );
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
                let mut out = format!(
                    "{indent}for (const {var_name} in {}) {{\n",
                    self.generate_expr(object)
                );
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
                    out.push_str(&self.case_block(body));
                }

                if let Some(default_body) = default {
                    out.push_str(&format!("{case_indent}default:\n"));
                    out.push_str(&self.case_block(default_body));
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
                let mut methods_out = String::new();
                for method in methods {
                    let method_indent = self.current_indent();
                    // `parse scheme start() {}` is no method name; a key that
                    // is not an identifier is written as a string.
                    let key = if crate::util::is_valid_identifier(&method.key)
                        || crate::util::is_valid_identifier(method.key.trim_start_matches('#'))
                    {
                        method.key.clone()
                    } else {
                        crate::util::escape_js_string(&method.key)
                    };
                    if method.is_static {
                        methods_out.push_str(&format!("{method_indent}static "));
                    } else {
                        methods_out.push_str(&method_indent);
                    }

                    // Handle method kind (getter/setter)
                    let kind_prefix = match method.kind {
                        crate::ir::MethodKind::Getter => "get ",
                        crate::ir::MethodKind::Setter => "set ",
                        _ => "",
                    };

                    // The whole-program rendering of the method's function (named,
                    // inlined, closure-resolved) is the better body; the IR body
                    // the class analyzer fetched went through the per-function
                    // pipeline only. Take the rendered function and rewrite its
                    // head into method syntax.
                    let from_inline = match &method.value {
                        crate::ir::Expression::Function { id, .. }
                            if self.inline_bodies.contains_key(&id.0) =>
                        {
                            let text = self.generate_expr(&method.value);
                            (!text.contains(super::BODY_HOLE))
                                .then(|| method_from_function_text(&text, &key))
                                .flatten()
                        }
                        _ => None,
                    };
                    if let Some(text) = from_inline {
                        methods_out.push_str(kind_prefix);
                        methods_out.push_str(&text);
                        methods_out.push('\n');
                    } else if let crate::ir::Expression::Function {
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
                            methods_out.push_str(&format!(
                                "{kind_prefix}{async_prefix}{gen}{key}({params}) {{\n"
                            ));
                            self.indent_level += 1;
                            methods_out.push_str(&self.generate_statements(body));
                            self.indent_level -= 1;
                            methods_out.push_str(&format!("{method_indent}}}\n"));
                        } else {
                            methods_out.push_str(&format!(
                                "{kind_prefix}{async_prefix}{gen}{key}({params}) {{ /* compiled code */ }}\n"
                            ));
                        }
                    } else {
                        // Fallback
                        let id = match &method.value {
                            crate::ir::Expression::Function { id, .. } => id.0,
                            _ => 0,
                        };
                        methods_out
                            .push_str(&format!("{kind_prefix}{key}() {}\n", super::body_hole(id)));
                    }
                }

                // A private name read anywhere in the class body must be declared
                // by the class; the bytecode carries no declaration, only the
                // reads. Nested closures are already text here, so the rendered
                // body is scanned as well as the IR. A private method declares
                // its own name.
                {
                    let mut names: std::collections::BTreeSet<String> =
                        std::collections::BTreeSet::new();
                    for method in methods {
                        if let Some(body) = &method.body {
                            collect_private_names(body, &mut names);
                        }
                    }
                    collect_private_names_in_text(&methods_out, &mut names);
                    for method in methods {
                        if method.key.starts_with('#') {
                            names.remove(&method.key);
                        }
                    }
                    let field_indent = self.current_indent();
                    for private in names {
                        out.push_str(&format!("{field_indent}{private};\n"));
                    }
                }
                out.push_str(&methods_out);
                self.indent_level -= 1;
                out.push_str(&format!("{indent}}}\n"));
                out
            }
        }
    }
}

// Rewrite a rendered function expression into a class method named `key`:
// `async function f(a) {…}` becomes `async key(a) {…}`, `function* f()`
// becomes `*key()`, `(a) => {…}` becomes `key(a) {…}` and a concise arrow
// `(a) => expr` becomes `key(a) { return expr; }`. `None` when the text has
// no shape this understands, so the caller falls back to the IR body.
fn method_from_function_text(text: &str, key: &str) -> Option<String> {
    let (is_async, rest) = match text.strip_prefix("async ") {
        Some(r) => (true, r),
        None => (false, text),
    };
    let async_prefix = if is_async { "async " } else { "" };
    if let Some(r) = rest.strip_prefix("function") {
        let (star, r) = match r.strip_prefix('*') {
            Some(r) => ("*", r),
            None => ("", r),
        };
        // `function name(` or `function(`: everything from the parameter list on
        // is kept as is.
        let paren = r.find('(')?;
        let head = r[..paren].trim();
        if !head.is_empty() && !crate::util::is_valid_identifier(head) {
            return None;
        }
        return Some(format!("{async_prefix}{star}{key}{}", &r[paren..]));
    }
    // Arrow: `(params) => body`. The renderer always parenthesises params.
    if !rest.starts_with('(') {
        return None;
    }
    let arrow = rest.find(") =>")?;
    let params = &rest[..=arrow];
    let body = rest[arrow + ") =>".len()..].trim_start();
    if body.starts_with('{') {
        return Some(format!("{async_prefix}{key}{params} {body}"));
    }
    // Concise body: its lines are already indented for the method's level, so
    // the return line is pushed one level in and the brace closes at the
    // method's indent, which is the indent of the text's last line.
    let last_indent: String = text
        .lines()
        .last()
        .map(|l| l.chars().take_while(|c| *c == ' ').collect())
        .unwrap_or_default();
    let body = body
        .strip_prefix('(')
        .and_then(|b| b.strip_suffix(')'))
        .unwrap_or(body);
    Some(format!(
        "{async_prefix}{key}{params} {{\n{last_indent}  return {body};\n{last_indent}}}"
    ))
}

// Every `#name` a rendered class body reads: `x.#name`, `x?.#name` and
// `#name in x`. Nested function bodies are text by the time a class renders,
// so this is the only view of the reads inside them.
fn collect_private_names_in_text(text: &str, out: &mut std::collections::BTreeSet<String>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(rel) = text[i..].find('#') {
        let at = i + rel;
        let name_end = at
            + 1
            + text[at + 1..]
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
                .unwrap_or(text.len() - at - 1);
        i = at + 1;
        let name = &text[at + 1..name_end];
        if name.is_empty() || name.as_bytes()[0].is_ascii_digit() {
            continue;
        }
        let after_dot = at > 0 && bytes[at - 1] == b'.';
        let before_in = text[name_end..].starts_with(" in ");
        if after_dot || before_in {
            out.insert(format!("#{name}"));
        }
    }
}

// Every `#name` read through a member access in these statements, nested
// bodies included; a class declares them once at its top.
fn collect_private_names(stmts: &[Statement], out: &mut std::collections::BTreeSet<String>) {
    use crate::ir::{Expression, Visitor};
    struct C<'a>(&'a mut std::collections::BTreeSet<String>);
    impl<'a, 'b> Visitor<'b> for C<'a> {
        fn visit_expression(&mut self, e: &'b Expression) {
            if let Expression::Member { property, .. } = e {
                if let Some(name) = crate::ir::expr::display::private_field_name_of(property) {
                    self.0.insert(format!("#{}", name.trim_start_matches('#')));
                }
            }
            self.walk_expression(e);
        }
    }
    let mut c = C(out);
    for s in stmts {
        c.visit_statement(s);
    }
}

#[cfg(test)]
mod method_text_tests {
    use super::method_from_function_text;

    #[test]
    fn function_heads_become_method_heads() {
        assert_eq!(
            method_from_function_text("function one(a, b) {\n  return a;\n}", "one"),
            Some("one(a, b) {\n  return a;\n}".into())
        );
        assert_eq!(
            method_from_function_text("async function f() {\n}", "load"),
            Some("async load() {\n}".into())
        );
        assert_eq!(
            method_from_function_text("function* f() {\n}", "items"),
            Some("*items() {\n}".into())
        );
        assert_eq!(
            method_from_function_text("function() {\n}", "\"a b\""),
            Some("\"a b\"() {\n}".into())
        );
    }

    #[test]
    fn arrows_become_methods() {
        assert_eq!(
            method_from_function_text("(x) => {\n  return x;\n}", "id"),
            Some("id(x) {\n  return x;\n}".into())
        );
        assert_eq!(
            method_from_function_text("async (x) => x.y", "get"),
            Some("async get(x) {\n  return x.y;\n}".into())
        );
        assert_eq!(
            method_from_function_text("() => ({ a: 1 })", "make"),
            Some("make() {\n  return { a: 1 };\n}".into())
        );
        assert_eq!(method_from_function_text("x => x", "id"), None);
    }
}

#[cfg(test)]
mod private_name_tests {
    use super::collect_private_names_in_text;

    #[test]
    fn private_reads_in_rendered_text_are_declared() {
        let text = "one(x) {\n  return (y) => y.#r.size > 0 && #s in y && a?.#t;\n}\n// #comment and \"#fff\" stay out";
        let mut names = std::collections::BTreeSet::new();
        collect_private_names_in_text(text, &mut names);
        let got: Vec<_> = names.into_iter().collect();
        assert_eq!(got, vec!["#r", "#s", "#t"]);
    }
}
