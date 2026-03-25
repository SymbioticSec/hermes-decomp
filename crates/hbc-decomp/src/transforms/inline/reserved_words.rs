use crate::ir::{AssignTarget, Expression, Statement, Value};

// Reserved JS keywords that cannot be used as variable names.
const JS_RESERVED: &[&str] = &[
    // Keywords
    "break", "case", "catch", "class", "const", "continue", "debugger", "default",
    "delete", "do", "else", "enum", "export", "extends", "finally", "for",
    "function", "if", "import", "in", "instanceof", "let", "new", "return",
    "super", "switch", "this", "throw", "try", "typeof", "var", "void",
    "while", "with", "yield", "await",
    // Literals
    "true", "false", "null", "undefined",
    // Strict mode / future reserved
    "implements", "interface", "package", "private", "protected", "public", "static",
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
        Statement::If { condition, then_body, else_body } => {
            rename_reserved_in_expr(condition);
            for s in then_body { rename_reserved_in_stmt(s); }
            for s in else_body { rename_reserved_in_stmt(s); }
        }
        Statement::While { condition, body } => {
            rename_reserved_in_expr(condition);
            for s in body { rename_reserved_in_stmt(s); }
        }
        Statement::For { init, condition, update, body } => {
            if let Some(i) = init { rename_reserved_in_stmt(i); }
            if let Some(c) = condition { rename_reserved_in_expr(c); }
            if let Some(u) = update { rename_reserved_in_stmt(u); }
            for s in body { rename_reserved_in_stmt(s); }
        }
        Statement::ForIn { object, body, .. } => {
            rename_reserved_in_expr(object);
            for s in body { rename_reserved_in_stmt(s); }
        }
        Statement::ForOf { iterable, body, .. } => {
            rename_reserved_in_expr(iterable);
            for s in body { rename_reserved_in_stmt(s); }
        }
        Statement::Block(inner) => {
            for s in inner { rename_reserved_in_stmt(s); }
        }
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
            for s in try_body { rename_reserved_in_stmt(s); }
            for s in catch_body { rename_reserved_in_stmt(s); }
            for s in finally_body { rename_reserved_in_stmt(s); }
        }
        Statement::Switch { discriminant, cases, default } => {
            rename_reserved_in_expr(discriminant);
            for (e, body) in cases {
                rename_reserved_in_expr(e);
                for s in body { rename_reserved_in_stmt(s); }
            }
            if let Some(d) = default { for s in d { rename_reserved_in_stmt(s); } }
        }
        _ => {}
    }
}

fn rename_reserved_in_target(target: &mut AssignTarget) {
    match target {
        AssignTarget::Variable(name) => {
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
        Expression::Value(Value::Variable(name)) => {
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
            for a in arguments { rename_reserved_in_expr(a); }
        }
        Expression::New { callee, arguments } => {
            rename_reserved_in_expr(callee);
            for a in arguments { rename_reserved_in_expr(a); }
        }
        Expression::Member { object, .. } => rename_reserved_in_expr(object),
        Expression::Conditional { condition, then_expr, else_expr } => {
            rename_reserved_in_expr(condition);
            rename_reserved_in_expr(then_expr);
            rename_reserved_in_expr(else_expr);
        }
        Expression::Array { elements } => {
            for e in elements.iter_mut().flatten() { rename_reserved_in_expr(e); }
        }
        Expression::Object { properties } => {
            for p in properties { rename_reserved_in_expr(&mut p.value); }
        }
        Expression::Assignment { target, value } => {
            rename_reserved_in_expr(target);
            rename_reserved_in_expr(value);
        }
        Expression::Spread(inner) => rename_reserved_in_expr(inner),
        Expression::TemplateLiteral { expressions, .. } => {
            for e in expressions { rename_reserved_in_expr(e); }
        }
        Expression::Yield { value, .. } => rename_reserved_in_expr(value),
        Expression::Await(inner) => rename_reserved_in_expr(inner),
        _ => {}
    }
}

fn is_reserved(name: &str) -> bool {
    JS_RESERVED.contains(&name)
}
