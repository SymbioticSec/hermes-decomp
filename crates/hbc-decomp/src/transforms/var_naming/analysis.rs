use super::state::{infer_name_from_expr, VariableNamer};
use crate::ir::{AssignTarget, Expression, PropertyKey, Statement, Value};

pub fn analyze_stmt(namer: &mut VariableNamer, stmt: &Statement) {
    match stmt {
        Statement::Assign { target, value } => {
            // Handle Register for backward compat
            if let AssignTarget::Register(r) = target {
                if let Some(name) = infer_name_from_expr(value) {
                    log::debug!("Register r{r} inferred name '{name}' from expression");
                    namer.suggest_name(&format!("r{r}"), &name);
                }
            }
            // Handle Variables (r0, r1, etc).
            // We only rename variables that are clearly generic/generated.
            // If a variable already has a meaningful name (e.g. from debug info or closure analysis), we keep it.
            if let AssignTarget::Variable(v) = target {
                // Check if the name is generic and can be improved
                let is_generic = (v.starts_with('r') && v[1..].chars().all(|c| c.is_ascii_digit()))
                    || v.starts_with("closure_")
                    || v == "obj"
                    || v.starts_with("obj")
                    || v == "arr"
                    || v.starts_with("arr")
                    || v == "tmp"
                    || v.starts_with("tmp")
                    || v == "val"
                    || v.starts_with("val")
                    || v == "promise"
                    || v.starts_with("promise");

                if is_generic {
                    if let Some(name) = infer_name_from_expr(value) {
                        // Don't rename if we infer the same generic name
                        if name != *v && !name.starts_with(v) {
                            log::debug!(
                                "Variable '{v}' inferred name '{name}' from expression"
                            );
                            namer.suggest_name(v, &name);
                        }
                    }
                }
            }
        }
        Statement::Let { value, .. } => {
            analyze_expr(namer, value);
        }
        Statement::If {
            condition,
            then_body,
            else_body,
        } => {
            analyze_expr(namer, condition);
            for s in then_body {
                analyze_stmt(namer, s);
            }
            for s in else_body {
                analyze_stmt(namer, s);
            }
        }
        Statement::While { condition, body } | Statement::DoWhile { body, condition } => {
            analyze_expr(namer, condition);
            for s in body {
                analyze_stmt(namer, s);
            }
        }
        Statement::For {
            init,
            condition,
            update,
            body,
        } => {
            if let Some(s) = init {
                analyze_stmt(namer, s);
            }
            if let Some(e) = condition {
                analyze_expr(namer, e);
            }
            if let Some(s) = update {
                analyze_stmt(namer, s);
            }
            for s in body {
                analyze_stmt(namer, s);
            }
        }
        Statement::ForOf { iterable, body, .. } => {
            analyze_expr(namer, iterable);
            for s in body {
                analyze_stmt(namer, s);
            }
        }
        Statement::ForIn { object, body, .. } => {
            analyze_expr(namer, object);
            for s in body {
                analyze_stmt(namer, s);
            }
        }
        Statement::Return(Some(e)) | Statement::Throw(e) | Statement::Expr(e) => {
            analyze_expr(namer, e);
        }
        Statement::TryCatch {
            try_body,
            catch_body,
            finally_body,
            ..
        } => {
            for s in try_body {
                analyze_stmt(namer, s);
            }
            for s in catch_body {
                analyze_stmt(namer, s);
            }
            for s in finally_body {
                analyze_stmt(namer, s);
            }
        }
        Statement::Switch {
            discriminant,
            cases,
            default,
        } => {
            analyze_expr(namer, discriminant);
            for (e, stmts) in cases {
                analyze_expr(namer, e);
                for s in stmts {
                    analyze_stmt(namer, s);
                }
            }
            if let Some(stmts) = default {
                for s in stmts {
                    analyze_stmt(namer, s);
                }
            }
        }
        Statement::Block(stmts) => {
            for s in stmts {
                analyze_stmt(namer, s);
            }
        }
        _ => {}
    }
}

pub fn analyze_expr(namer: &mut VariableNamer, expr: &Expression) {
    analyze_expr_with_suggestion(namer, expr, None);
}

// Recursively analyzes an expression to propagate naming suggestions.
//
// `suggestion` is an optional name hint coming from the parent context.
// Example: `var r0 = { email: r1 };`
// - analyzing `{ email: r1 }` passes "r0" (unused here)
// - analyzing property `email: r1` passes "email" as suggestion to `r1`.
fn analyze_expr_with_suggestion(
    namer: &mut VariableNamer,
    expr: &Expression,
    suggestion: Option<&str>,
) {
    match expr {
        Expression::Value(Value::Variable(var_name)) => {
            if let Some(s) = suggestion {
                namer.suggest_name(var_name, s);
            }
        }
        Expression::Value(Value::Parameter(idx)) => {
            if let Some(s) = suggestion {
                namer.suggest_name(&format!("arg{idx}"), s);
            }
        }
        Expression::Call { callee, arguments } => {
            analyze_expr_with_suggestion(namer, callee, None);
            for arg in arguments {
                analyze_expr_with_suggestion(namer, arg, suggestion);
            }
        }
        Expression::New { callee, arguments } => {
            analyze_expr_with_suggestion(namer, callee, None);
            for arg in arguments {
                analyze_expr_with_suggestion(namer, arg, suggestion);
            }
        }
        Expression::Member {
            object, property, ..
        } => {
            if let (Expression::Value(val), PropertyKey::Ident(prop)) = (&**object, property) {
                // Skip common array/string methods - they don't describe what the variable contains
                if crate::constants::is_transformation_method(prop) {
                    // Don't rename based on method access
                } else {
                    let var_name = match val {
                        Value::Variable(name) => Some(name.clone()),
                        Value::Register(r) => Some(format!("r{r}")),
                        Value::Parameter(idx) => Some(format!("arg{idx}")),
                        _ => None,
                    };

                    if let Some(v) = var_name {
                        log::debug!(
                            "Property access '.{prop}' suggests name for variable '{v}'"
                        );
                        namer.suggest_name_from_property(&v, prop);
                    }
                }
            }
            analyze_expr_with_suggestion(namer, object, suggestion);
        }
        Expression::Binary { left, right, .. } => {
            analyze_expr_with_suggestion(namer, left, suggestion);
            analyze_expr_with_suggestion(namer, right, suggestion);
        }
        Expression::Unary { operand, .. } => {
            analyze_expr_with_suggestion(namer, operand, suggestion);
        }
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            analyze_expr_with_suggestion(namer, condition, None);
            analyze_expr_with_suggestion(namer, then_expr, suggestion);
            analyze_expr_with_suggestion(namer, else_expr, suggestion);
        }
        Expression::Array { elements } => {
            for elem in elements.iter().flatten() {
                analyze_expr_with_suggestion(namer, elem, suggestion);
            }
        }
        Expression::Object { properties } => {
            for prop in properties {
                if let PropertyKey::Ident(key_name) = &prop.key {
                    analyze_expr_with_suggestion(namer, &prop.value, Some(key_name));
                } else {
                    analyze_expr_with_suggestion(namer, &prop.value, None);
                }
            }
        }
        _ => {}
    }
}
