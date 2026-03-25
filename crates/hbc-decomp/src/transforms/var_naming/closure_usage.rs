use super::state::VariableNamer;
use crate::ir::{AssignTarget, Expression, PropertyKey, Statement, Value};
use std::collections::BTreeMap;

// Infer closure names from their usage context.
// Analyzes how closures are used (e.g., passed to .then(), .map(), .filter())
// to suggest better variable names.
pub fn infer_closure_names_from_usage(stmts: &[Statement], namer: &mut VariableNamer) {
    for stmt in stmts {
        infer_closure_names_in_stmt(stmt, namer);
    }
}

fn infer_closure_names_in_stmt(stmt: &Statement, namer: &mut VariableNamer) {
    match stmt {
        Statement::Assign { value, .. } => {
            // Check for closures passed to methods like .then(), .map(), etc.
            if let Expression::Call { callee, arguments } = value {
                if let Some(method_name) = extract_method_name(callee) {
                    for (idx, arg) in arguments.iter().enumerate() {
                        if let Expression::Function { .. } = arg {
                            if let Some(name) = closure_name_from_method(&method_name, idx) {
                                // Try to find the register for this closure
                                if let Some(reg) = find_closure_register(arg, stmt) {
                                    namer.suggest_name(&format!("r{reg}"), &name);
                                }
                            }
                        }
                    }
                }
            }
        }
        Statement::Block(stmts) => {
            for s in stmts {
                infer_closure_names_in_stmt(s, namer);
            }
        }
        Statement::If { then_body, else_body, .. } => {
            for s in then_body { infer_closure_names_in_stmt(s, namer); }
            for s in else_body { infer_closure_names_in_stmt(s, namer); }
        }
        Statement::While { body, .. }
        | Statement::DoWhile { body, .. }
        | Statement::For { body, .. }
        | Statement::ForOf { body, .. }
        | Statement::ForIn { body, .. } => {
            for s in body { infer_closure_names_in_stmt(s, namer); }
        }
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
            for s in try_body { infer_closure_names_in_stmt(s, namer); }
            for s in catch_body { infer_closure_names_in_stmt(s, namer); }
            for s in finally_body { infer_closure_names_in_stmt(s, namer); }
        }
        Statement::Switch { cases, default, .. } => {
            for (_, stmts) in cases {
                for s in stmts { infer_closure_names_in_stmt(s, namer); }
            }
            if let Some(stmts) = default {
                for s in stmts { infer_closure_names_in_stmt(s, namer); }
            }
        }
        _ => {}
    }
}

// Extract method name from a callee expression (e.g., `.then` from `promise.then`)
fn extract_method_name(callee: &Expression) -> Option<String> {
    if let Expression::Member { property, .. } = callee {
        match property {
            PropertyKey::Ident(name) | PropertyKey::String(name) => Some(name.clone()),
            _ => None,
        }
    } else {
        None
    }
}

// Get a closure name based on the method it's passed to
fn closure_name_from_method(method_name: &str, arg_index: usize) -> Option<String> {
    match method_name {
        "then" if arg_index == 0 => Some("onFulfilled".to_string()),
        "then" if arg_index == 1 => Some("onRejected".to_string()),
        "catch" => Some("onRejected".to_string()),
        "finally" => Some("onFinally".to_string()),
        "map" => Some("mapper".to_string()),
        "filter" => Some("predicate".to_string()),
        "reduce" => Some("reducer".to_string()),
        "forEach" => Some("callback".to_string()),
        "find" => Some("predicate".to_string()),
        "some" => Some("predicate".to_string()),
        "every" => Some("predicate".to_string()),
        "sort" => Some("comparator".to_string()),
        _ => None,
    }
}

// Try to find the register associated with a closure expression
fn find_closure_register(_closure: &Expression, _stmt: &Statement) -> Option<u32> {
    None
}

// Usage information collected for a single closure variable.
#[derive(Debug, Default)]
pub(super) struct ClosureUsageInfo {
    // Properties accessed on this closure (e.g., "setToken", "getUser", "default")
    pub properties: Vec<String>,
    // Methods called on this closure (e.g., "setToken", "push", "then")
    pub methods: Vec<String>,
    // Whether this closure is called directly as a function (e.g., `closure_12(x)`)
    pub called_as_function: bool,
}

// Check if a variable name is a generic closure name (closure_N pattern).
pub(super) fn is_closure_name(name: &str) -> bool {
    if let Some(suffix) = name.strip_prefix("closure_") {
        suffix.chars().all(|c| c.is_ascii_digit()) && !suffix.is_empty()
    } else {
        false
    }
}

// Collect usage information for all closure_N variables in a statement tree.
pub(super) fn collect_closure_usage_in_stmt(stmt: &Statement, usage: &mut BTreeMap<String, ClosureUsageInfo>) {
    match stmt {
        Statement::Assign { target, value } => {
            collect_closure_usage_in_target(target, usage);
            collect_closure_usage_in_expr(value, usage);
        }
        Statement::Let { value, .. } => {
            collect_closure_usage_in_expr(value, usage);
        }
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
            collect_closure_usage_in_expr(e, usage);
        }
        Statement::If { condition, then_body, else_body } => {
            collect_closure_usage_in_expr(condition, usage);
            for s in then_body { collect_closure_usage_in_stmt(s, usage); }
            for s in else_body { collect_closure_usage_in_stmt(s, usage); }
        }
        Statement::While { condition, body } | Statement::DoWhile { body, condition } => {
            collect_closure_usage_in_expr(condition, usage);
            for s in body { collect_closure_usage_in_stmt(s, usage); }
        }
        Statement::For { init, condition, update, body } => {
            if let Some(s) = init { collect_closure_usage_in_stmt(s, usage); }
            if let Some(e) = condition { collect_closure_usage_in_expr(e, usage); }
            if let Some(s) = update { collect_closure_usage_in_stmt(s, usage); }
            for s in body { collect_closure_usage_in_stmt(s, usage); }
        }
        Statement::ForOf { iterable, body, .. } => {
            collect_closure_usage_in_expr(iterable, usage);
            for s in body { collect_closure_usage_in_stmt(s, usage); }
        }
        Statement::ForIn { object, body, .. } => {
            collect_closure_usage_in_expr(object, usage);
            for s in body { collect_closure_usage_in_stmt(s, usage); }
        }
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
            for s in try_body { collect_closure_usage_in_stmt(s, usage); }
            for s in catch_body { collect_closure_usage_in_stmt(s, usage); }
            for s in finally_body { collect_closure_usage_in_stmt(s, usage); }
        }
        Statement::Switch { discriminant, cases, default } => {
            collect_closure_usage_in_expr(discriminant, usage);
            for (e, stmts) in cases {
                collect_closure_usage_in_expr(e, usage);
                for s in stmts { collect_closure_usage_in_stmt(s, usage); }
            }
            if let Some(stmts) = default {
                for s in stmts { collect_closure_usage_in_stmt(s, usage); }
            }
        }
        Statement::Block(stmts) => {
            for s in stmts { collect_closure_usage_in_stmt(s, usage); }
        }
        _ => {}
    }
}

fn collect_closure_usage_in_target(target: &AssignTarget, usage: &mut BTreeMap<String, ClosureUsageInfo>) {
    match target {
        AssignTarget::Member { object, .. } => {
            collect_closure_usage_in_expr(object, usage);
        }
        AssignTarget::Index { object, key } => {
            collect_closure_usage_in_expr(object, usage);
            collect_closure_usage_in_expr(key, usage);
        }
        _ => {}
    }
}

// Function invocation methods (not domain-specific — exclude from method analysis).
fn is_invocation_method(name: &str) -> bool {
    matches!(name, "call" | "apply" | "bind")
}

fn collect_closure_usage_in_expr(expr: &Expression, usage: &mut BTreeMap<String, ClosureUsageInfo>) {
    match expr {
        // closure_N.property (read)
        Expression::Member { object, property, .. } => {
            if let Expression::Value(Value::Variable(name)) = &**object {
                if is_closure_name(name) {
                    if let Some(prop) = ident_from_property(property) {
                        if !is_invocation_method(&prop) {
                            let info = usage.entry(name.clone()).or_default();
                            info.properties.push(prop);
                        }
                    }
                }
            }
            // Recurse
            collect_closure_usage_in_expr(object, usage);
        }
        // closure_N(args) — direct function call
        Expression::Call { callee, arguments } => {
            // Check for closure_N.method(args) — method call
            if let Expression::Member { object, property, .. } = &**callee {
                if let Expression::Value(Value::Variable(name)) = &**object {
                    if is_closure_name(name) {
                        if let Some(method) = ident_from_property(property) {
                            if is_invocation_method(&method) {
                                // closure_N.call(...) / .apply(...) → treat as bare function call
                                usage.entry(name.clone()).or_default().called_as_function = true;
                            } else {
                                let info = usage.entry(name.clone()).or_default();
                                info.methods.push(method);
                            }
                        }
                    }
                }
            }
            // Check for closure_N(args) — bare call
            if let Expression::Value(Value::Variable(name)) = &**callee {
                if is_closure_name(name) {
                    usage.entry(name.clone()).or_default().called_as_function = true;
                }
            }
            collect_closure_usage_in_expr(callee, usage);
            for arg in arguments {
                collect_closure_usage_in_expr(arg, usage);
            }
        }
        Expression::New { callee, arguments } => {
            collect_closure_usage_in_expr(callee, usage);
            for arg in arguments { collect_closure_usage_in_expr(arg, usage); }
        }
        Expression::Binary { left, right, .. } => {
            collect_closure_usage_in_expr(left, usage);
            collect_closure_usage_in_expr(right, usage);
        }
        Expression::Unary { operand, .. } => {
            collect_closure_usage_in_expr(operand, usage);
        }
        Expression::Conditional { condition, then_expr, else_expr } => {
            collect_closure_usage_in_expr(condition, usage);
            collect_closure_usage_in_expr(then_expr, usage);
            collect_closure_usage_in_expr(else_expr, usage);
        }
        Expression::Array { elements } => {
            for e in elements.iter().flatten() { collect_closure_usage_in_expr(e, usage); }
        }
        Expression::Object { properties } => {
            for p in properties { collect_closure_usage_in_expr(&p.value, usage); }
        }
        Expression::Assignment { target, value } => {
            collect_closure_usage_in_expr(target, usage);
            collect_closure_usage_in_expr(value, usage);
        }
        Expression::Spread(inner) | Expression::Await(inner) => {
            collect_closure_usage_in_expr(inner, usage);
        }
        Expression::Yield { value, .. } => {
            collect_closure_usage_in_expr(value, usage);
        }
        Expression::TemplateLiteral { expressions, .. } => {
            for e in expressions { collect_closure_usage_in_expr(e, usage); }
        }
        _ => {}
    }
}

pub(super) fn ident_from_property(prop: &PropertyKey) -> Option<String> {
    match prop {
        PropertyKey::Ident(name) | PropertyKey::String(name) => Some(name.clone()),
        _ => None,
    }
}
