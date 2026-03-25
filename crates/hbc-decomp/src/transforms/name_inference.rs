// Name inference pass.
//
// Tries to infer names for anonymous functions based on where they are assigned.
// e.g. `obj.foo = function() {...}` -> `function foo() {...}`
// Also infers names for common patterns (e.g. `x = []` -> `arr`).

use crate::ir::{AssignTarget, Expression, Statement, Value};

pub fn infer_names(statements: &mut [Statement]) {
    for stmt in statements {
        infer_stmt(stmt);
    }
}

fn infer_stmt(stmt: &mut Statement) {
    match stmt {
        Statement::Assign { target, value } => {
            // 1. Function naming from target
            if let Some(name) = extract_name_from_target(target) {
                apply_name(value, name);
            }

            // 2. Variable naming from Value (Type Inference)
            if let AssignTarget::Register(_r) = target {
                // Nothing to do for register unless we have a map of reg->name.
                // But structure analysis transforms registers to vars later?
                // No, registers are used until renamed by naming pass.
                // Wait, this pass infers names for FUNCTIONS inside values.
                // It doesn't rename variables.
            }

            // 3. Infer target variable name from value pattern (heuristic)
            if let AssignTarget::Variable(ref mut name) = target {
                if name.starts_with("r") || name.starts_with("val") {
                    if let Some(new_name) = suggest_name(value) {
                        *name = new_name;
                    }
                }
            }
        }
        Statement::If {
            then_body,
            else_body,
            ..
        } => {
            infer_names(then_body);
            infer_names(else_body);
        }
        Statement::While { body, .. } => infer_names(body),
        Statement::For { body, .. } => infer_names(body),
        Statement::Block(body) => infer_names(body),
        Statement::TryCatch {
            try_body,
            catch_body,
            finally_body,
            ..
        } => {
            infer_names(try_body);
            infer_names(catch_body);
            infer_names(finally_body);
        }
        _ => {}
    }
}

fn extract_name_from_target(target: &AssignTarget) -> Option<String> {
    match target {
        AssignTarget::Variable(name) => Some(name.clone()),
        AssignTarget::Member { property, .. } => Some(property.clone()),
        _ => None,
    }
}

fn apply_name(expr: &mut Expression, name: String) {
    if let Expression::Function {
        name: func_name, ..
    } = expr
    {
        if func_name.is_none() {
            *func_name = Some(name);
        }
    }
}

// Simple type inference heuristics
fn suggest_name(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Array { .. } => Some("arr".to_string()),
        Expression::Object { .. } => Some("obj".to_string()),
        Expression::New { callee, .. } => {
            if let Expression::Value(Value::Variable(cls)) = &**callee {
                Some(cls.to_lowercase())
            } else {
                Some("inst".to_string())
            }
        }
        Expression::Call { callee, .. } => {
            if let Expression::Member { property, .. } = &**callee {
                let prop_name = match property {
                    crate::ir::PropertyKey::Ident(s) | crate::ir::PropertyKey::String(s) => {
                        Some(s.as_str())
                    }
                    _ => None,
                };

                if let Some(name) = prop_name {
                    match name {
                        "map" | "filter" | "reduce" | "slice" => Some("arr".to_string()),
                        "then" | "catch" | "finally" => Some("promise".to_string()),
                        _ => None,
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None,
    }
}
