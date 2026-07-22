use super::{Codegen, is_effectively_empty, sanitize_loop_var, replace_whole_word};
use crate::ir::Statement;

impl Codegen {
    pub(super) fn generate_if(
        &mut self,
        condition: &crate::ir::Expression,
        then_body: &[Statement],
        else_body: &[Statement],
    ) -> String {
        // Safety net: skip completely empty if blocks
        let then_empty = then_body.is_empty() || is_effectively_empty(then_body);
        let else_empty = else_body.is_empty() || is_effectively_empty(else_body);
        if then_empty && else_empty {
            return String::new();
        }

        let indent = self.current_indent();
        let mut out = format!("{indent}if ({}) {{\n", self.generate_expr(condition));

        self.indent_level += 1;
        out.push_str(&self.generate_statements(then_body));
        self.indent_level -= 1;

        if else_body.is_empty() || is_effectively_empty(else_body) {
            out.push_str(&format!("{indent}}}\n"));
        } else if else_body.len() == 1 {
            if let Statement::If { condition: else_cond, then_body: else_then, else_body: else_else } = &else_body[0] {
                // `else { if (...) { ... } }` -> `else if (...) { ... }`
                let inner = self.generate_if(else_cond, else_then, else_else);
                if inner.is_empty() {
                    // Inner if was completely empty, just close the outer block
                    out.push_str(&format!("{indent}}}\n"));
                } else {
                    out.push_str(&format!("{indent}}} else "));
                    out.push_str(inner.trim_start());
                }
            } else {
                out.push_str(&format!("{indent}}} else {{\n"));
                self.indent_level += 1;
                out.push_str(&self.generate_statements(else_body));
                self.indent_level -= 1;
                out.push_str(&format!("{indent}}}\n"));
            }
        } else {
            out.push_str(&format!("{indent}}} else {{\n"));
            self.indent_level += 1;
            out.push_str(&self.generate_statements(else_body));
            self.indent_level -= 1;
            out.push_str(&format!("{indent}}}\n"));
        }

        out
    }

    pub(super) fn generate_while(&mut self, condition: &crate::ir::Expression, body: &[Statement]) -> String {
        let indent = self.current_indent();

        // Empty loop body: while (cond) {}
        if is_effectively_empty(body) {
            return format!("{indent}while ({}) {{}}\n", self.generate_expr(condition));
        }

        let mut out = format!("{indent}while ({}) {{\n", self.generate_expr(condition));

        self.indent_level += 1;
        out.push_str(&self.generate_statements(body));
        self.indent_level -= 1;

        out.push_str(&format!("{indent}}}\n"));
        out
    }

    pub(super) fn generate_do_while(
        &mut self,
        body: &[Statement],
        condition: &crate::ir::Expression,
    ) -> String {
        let indent = self.current_indent();
        let mut out = format!("{indent}do {{\n");

        self.indent_level += 1;
        out.push_str(&self.generate_statements(body));
        self.indent_level -= 1;

        out.push_str(&format!("{indent}}} while ({});\n", self.generate_expr(condition)));
        out
    }

    pub(super) fn generate_for(
        &mut self,
        init: Option<&Statement>,
        condition: Option<&crate::ir::Expression>,
        update: Option<&Statement>,
        body: &[Statement],
    ) -> String {
        let indent = self.current_indent();

        // Format init (without newline and semicolon)
        let init_str = match init {
            Some(Statement::Assign { target, value }) => format!("{} = {}", self.generate_assign_target(target), self.generate_expr(value)),
            Some(Statement::Let { name, value, kind }) => {
                if matches!(value, crate::ir::Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::Undefined))) {
                    format!("let {name}")
                } else {
                    format!("{kind} {name} = {}", self.generate_expr(value))
                }
            }
            _ => String::new(),
        };

        // Format condition
        let cond_str = condition.map(|c| self.generate_expr(c)).unwrap_or_default();

        // Format update (without newline and semicolon)
        let update_str = match update {
            Some(Statement::Assign { target, value }) => format!("{} = {}", self.generate_assign_target(target), self.generate_expr(value)),
            Some(Statement::Expr(e)) => self.generate_expr(e),
            _ => String::new(),
        };

        let mut out = format!("{indent}for ({init_str}; {cond_str}; {update_str}) {{\n");

        self.indent_level += 1;
        out.push_str(&self.generate_statements(body));
        self.indent_level -= 1;

        out.push_str(&format!("{indent}}}\n"));
        out
    }

    pub(super) fn generate_try_catch(
        &mut self,
        try_body: &[Statement],
        catch_param: Option<&str>,
        catch_body: &[Statement],
        finally_body: &[Statement],
    ) -> String {
        let indent = self.current_indent();
        let mut out = format!("{indent}try {{\n");

        self.indent_level += 1;
        out.push_str(&self.generate_statements(try_body));
        self.indent_level -= 1;

        if !catch_body.is_empty() || catch_param.is_some() {
            let raw_param = catch_param.unwrap_or("e");
            let param = sanitize_loop_var(raw_param, "err");
            out.push_str(&format!("{indent}}} catch ({param}) {{\n"));
            self.indent_level += 1;
            let catch_str = self.generate_statements(catch_body);
            if param != raw_param {
                out.push_str(&replace_whole_word(&catch_str, raw_param, &param));
            } else {
                out.push_str(&catch_str);
            }
            self.indent_level -= 1;
        }

        if !finally_body.is_empty() {
            out.push_str(&format!("{indent}}} finally {{\n"));
            self.indent_level += 1;
            out.push_str(&self.generate_statements(finally_body));
            self.indent_level -= 1;
        }

        out.push_str(&format!("{indent}}}\n"));
        out
    }
}
