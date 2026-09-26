use crate::ir::{AssignTarget, Binding, Expression, Statement, Value};

// Reserved JS keywords that cannot be used as variable names.
const JS_RESERVED: &[&str] = &[
    // Keywords
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "await",
    // Literals
    "true",
    "false",
    "null",
    "undefined",
    // Strict mode / future reserved
    "implements",
    "interface",
    "package",
    "private",
    "protected",
    "public",
    "static",
];

// Rename reserved JS keywords used as variable names.
// `default` -> `_default`, `new` -> `_new`, etc.
pub fn rename_reserved_words(stmts: &mut [Statement]) {
    for stmt in stmts.iter_mut() {
        rename_reserved_in_stmt(stmt);
    }
}

fn rename_reserved_in_stmt(stmt: &mut Statement) {
    match stmt {
        Statement::Assign { target, value } => {
            rename_reserved_in_target(target);
            rename_reserved_in_expr(value);
        }
        Statement::Let { name, value, .. } => {
            if is_reserved(name) {
                *name = format!("_{name}");
            }
            rename_reserved_in_expr(value);
        }
        Statement::Expr(e) => rename_reserved_in_expr(e),
        Statement::Return(Some(e)) | Statement::Throw(e) => rename_reserved_in_expr(e),
        Statement::If {
            condition,
            then_body,
            else_body,
        } => {
            rename_reserved_in_expr(condition);
            for s in then_body {
                rename_reserved_in_stmt(s);
            }
            for s in else_body {
                rename_reserved_in_stmt(s);
            }
        }
        Statement::While { condition, body } => {
            rename_reserved_in_expr(condition);
            for s in body {
                rename_reserved_in_stmt(s);
            }
        }
        Statement::For {
            init,
            condition,
            update,
            body,
        } => {
            if let Some(i) = init {
                rename_reserved_in_stmt(i);
            }
            if let Some(c) = condition {
                rename_reserved_in_expr(c);
            }
            if let Some(u) = update {
                rename_reserved_in_stmt(u);
            }
            for s in body {
                rename_reserved_in_stmt(s);
            }
        }
        Statement::ForIn { object, body, .. } => {
            rename_reserved_in_expr(object);
            for s in body {
                rename_reserved_in_stmt(s);
            }
        }
        Statement::ForOf { iterable, body, .. } => {
            rename_reserved_in_expr(iterable);
            for s in body {
                rename_reserved_in_stmt(s);
            }
        }
        Statement::Block(inner) => {
            for s in inner {
                rename_reserved_in_stmt(s);
            }
        }
        Statement::TryCatch {
            try_body,
            catch_body,
            finally_body,
            ..
        } => {
            for s in try_body {
                rename_reserved_in_stmt(s);
            }
            for s in catch_body {
                rename_reserved_in_stmt(s);
            }
            for s in finally_body {
                rename_reserved_in_stmt(s);
            }
        }
        Statement::Switch {
            discriminant,
            cases,
            default,
        } => {
            rename_reserved_in_expr(discriminant);
            for (e, body) in cases {
                rename_reserved_in_expr(e);
                for s in body {
                    rename_reserved_in_stmt(s);
                }
            }
            if let Some(d) = default {
                for s in d {
                    rename_reserved_in_stmt(s);
                }
            }
        }
        _ => {}
    }
}

fn rename_reserved_in_target(target: &mut AssignTarget) {
    match target {
        AssignTarget::Binding(Binding::Variable(name)) => {
            if is_reserved(name) {
                *name = format!("_{name}");
            }
        }
        AssignTarget::Member { object, .. } => rename_reserved_in_expr(object),
        AssignTarget::Index { object, key } => {
            rename_reserved_in_expr(object);
            rename_reserved_in_expr(key);
        }
        _ => {}
    }
}

fn rename_reserved_in_expr(expr: &mut Expression) {
    match expr {
        Expression::Value(Value::Binding(Binding::Variable(name))) => {
            if is_reserved(name) {
                *name = format!("_{name}");
            }
        }
        Expression::Binary { left, right, .. } => {
            rename_reserved_in_expr(left);
            rename_reserved_in_expr(right);
        }
        Expression::Unary { operand, .. } => rename_reserved_in_expr(operand),
        Expression::Call { callee, arguments } => {
            rename_reserved_in_expr(callee);
            for a in arguments {
                rename_reserved_in_expr(a);
            }
        }
        Expression::New { callee, arguments } => {
            rename_reserved_in_expr(callee);
            for a in arguments {
                rename_reserved_in_expr(a);
            }
        }
        Expression::Member { object, .. } => rename_reserved_in_expr(object),
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            rename_reserved_in_expr(condition);
            rename_reserved_in_expr(then_expr);
            rename_reserved_in_expr(else_expr);
        }
        Expression::Array { elements } => {
            for e in elements.iter_mut().flatten() {
                rename_reserved_in_expr(e);
            }
        }
        Expression::Object { properties } => {
            for p in properties {
                rename_reserved_in_expr(&mut p.value);
            }
        }
        Expression::Assignment { target, value } => {
            crate::ir::for_each_target_expression_mut(target, &mut rename_reserved_in_expr);
            rename_reserved_in_expr(value);
        }
        Expression::Spread(inner) => rename_reserved_in_expr(inner),
        Expression::TemplateLiteral { expressions, .. } => {
            for e in expressions {
                rename_reserved_in_expr(e);
            }
        }
        Expression::Yield { value, .. } => rename_reserved_in_expr(value),
        Expression::Await(inner) => rename_reserved_in_expr(inner),
        _ => {}
    }
}

fn is_reserved(name: &str) -> bool {
    JS_RESERVED.contains(&name)
}

// Two variables whose names differ only by what `sanitize_identifier` strips
// print as one identifier: a module named `react-native` gives a variable
// `react-native`, and a second import of the same library a variable
// `react_native`, so `import react_native` and `let react_native = …` bound
// the name twice. Each name that sanitises onto another variable's name is
// renamed to a fresh sanitised form before rendering.
pub fn make_sanitized_names_distinct(stmts: &mut [Statement]) {
    use crate::ir::Visitor;
    use std::collections::{BTreeMap, BTreeSet};
    struct Names(BTreeSet<String>);
    impl<'a> Visitor<'a> for Names {
        fn visit_expression(&mut self, e: &'a Expression) {
            if let Expression::Value(Value::Binding(Binding::Variable(n))) = e {
                self.0.insert(n.clone());
            }
            self.walk_expression(e);
        }
        fn visit_assign_target(&mut self, t: &'a AssignTarget) {
            if let AssignTarget::Binding(Binding::Variable(n)) = t {
                self.0.insert(n.clone());
            }
            self.walk_assign_target(t);
        }
        fn visit_binding_def(&mut self, name: &'a str) {
            self.0.insert(name.to_string());
        }
    }
    let mut names = Names(BTreeSet::new());
    for s in stmts.iter() {
        names.visit_statement(s);
    }
    let names = names.0;
    let mut by_sanitized: BTreeMap<String, Vec<&String>> = BTreeMap::new();
    for n in &names {
        by_sanitized
            .entry(crate::util::sanitize_identifier(n))
            .or_default()
            .push(n);
    }
    let mut taken: BTreeSet<String> = by_sanitized.keys().cloned().collect();
    let mut renames: BTreeMap<String, String> = BTreeMap::new();
    for (sanitized, group) in &by_sanitized {
        if group.len() < 2 {
            continue;
        }
        // The variable already spelled as the sanitised form keeps it; the
        // others take a numbered form.
        for n in group.iter().filter(|n| **n != sanitized) {
            let mut i = 2u32;
            let fresh = loop {
                let cand = format!("{sanitized}{i}");
                if !taken.contains(&cand) {
                    break cand;
                }
                i += 1;
            };
            taken.insert(fresh.clone());
            renames.insert((*n).clone(), fresh);
        }
    }
    if !renames.is_empty() {
        crate::analysis::naming::rename_variables_in_stmts(stmts, &renames);
    }
}

#[cfg(test)]
mod sanitized_names_tests {
    use super::*;

    #[test]
    fn a_hyphenated_name_no_longer_prints_as_an_existing_one() {
        let mut stmts = vec![
            Statement::Let {
                name: "react_native".into(),
                value: Expression::constant(crate::ir::Constant::Integer(1)),
                kind: crate::ir::VarKind::Const,
            },
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Variable("react-native".into())),
                value: Expression::Value(Value::Binding(Binding::Variable("react-native".into()))),
            },
        ];
        make_sanitized_names_distinct(&mut stmts);
        let text = format!("{:?}", stmts[1]);
        assert!(text.contains("react_native2"), "{text}");
        assert!(!text.contains("react-native"), "{text}");
    }
}
