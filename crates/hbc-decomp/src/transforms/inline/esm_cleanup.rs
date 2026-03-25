// ESM boilerplate removal and hoisted parameter alias inlining.

use crate::ir::{AssignTarget, Expression, Statement, Value};
use std::collections::BTreeMap;

use super::inline_named::is_inlinable_name;

// Remove ESM boilerplate patterns:
// 1. `x = { value: true }` followed by overwrite of x -> dead __esModule marker
// 2. `defineProperty(target, "__esModule", ...)` -> skip
// 3. `Object.defineProperty(target, "default", { enumerable: true, get: () => val })` -> skip (getter boilerplate)
pub(super) fn remove_esm_boilerplate(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut result = Vec::new();
    let len = stmts.len();

    let mut i = 0;
    while i < len {
        let stmt = &stmts[i];

        // Pattern: `x = { value: true }` followed by x = { enumerable: ... } -> skip first
        if i + 1 < len {
            if let Statement::Assign { target: AssignTarget::Variable(name), value } = stmt {
                if is_esmodule_marker_obj(value) {
                    // Check if next statement overwrites the same variable
                    if let Statement::Assign { target: AssignTarget::Variable(name2), .. } = &stmts[i + 1] {
                        if name == name2 {
                            i += 1;
                            continue;
                        }
                    }
                    // Or if it's used in a defineProperty with "__esModule"
                    if is_define_property_esmodule(&stmts[i + 1]) {
                        // Skip both the marker and the defineProperty
                        i += 2;
                        continue;
                    }
                }
            }
        }

        // Pattern: standalone defineProperty(target, "__esModule", ...) -> skip
        if is_define_property_esmodule(stmt) {
            i += 1;
            continue;
        }

        result.push(stmt.clone());
        i += 1;
    }

    result
}

// Check if an expression is `{ value: true }` (the __esModule marker object)
fn is_esmodule_marker_obj(expr: &Expression) -> bool {
    if let Expression::Object { properties } = expr {
        if properties.len() == 1 {
            let prop = &properties[0];
            if matches!(&prop.key, crate::ir::PropertyKey::Ident(k) | crate::ir::PropertyKey::String(k) if k == "value") {
                return matches!(&prop.value, Expression::Value(Value::Constant(crate::ir::Constant::Bool(true))));
            }
        }
    }
    false
}

// Check if a statement is a defineProperty call with "__esModule" as the property name
fn is_define_property_esmodule(stmt: &Statement) -> bool {
    let value = match stmt {
        Statement::Assign { value, .. } => value,
        Statement::Expr(value) => value,
        _ => return false,
    };

    if let Expression::Call { callee, arguments } = value {
        // Check callee is *.defineProperty
        let is_define_prop = match callee.as_ref() {
            Expression::Member { property: crate::ir::PropertyKey::Ident(p), .. } => p == "defineProperty",
            _ => false,
        };
        if is_define_prop && arguments.len() >= 3 {
            // Check for "__esModule" string in args (may be at index 1 or 2 depending on this-binding)
            for arg in arguments.iter().take(4) {
                if let Expression::Value(Value::Constant(crate::ir::Constant::String(s))) = arg {
                    if s == "__esModule" {
                        return true;
                    }
                }
            }
        }
    }
    false
}

// Find hoisted parameter aliases after return/throw, inline them into the body, then trim dead code.
// Pattern: `return X; const tmp = arg0; const tmp2 = arg1;` -> replace tmp->arg0, tmp2->arg1 in body.
pub(super) fn inline_hoisted_aliases_and_trim(stmts: Vec<Statement>) -> Vec<Statement> {
    // Find the first return/throw
    let term_idx = stmts.iter().position(|s| matches!(s, Statement::Return(_) | Statement::Throw(_)));
    let Some(term_idx) = term_idx else {
        return stmts;
    };

    // Collect aliases from statements after the terminator
    let mut aliases: BTreeMap<String, Expression> = BTreeMap::new();
    for stmt in &stmts[term_idx + 1..] {
        match stmt {
            // const tmp = arg0; or tmp = arg0;
            Statement::Assign { target: AssignTarget::Variable(name), value }
            | Statement::Let { name, value, .. } => {
                if is_inlinable_name(name) {
                    match value {
                        Expression::Value(Value::Parameter(_))
                        | Expression::Value(Value::Variable(_)) => {
                            aliases.insert(name.clone(), value.clone());
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // Truncate at the terminator (inclusive)
    let mut result: Vec<Statement> = stmts.into_iter().take(term_idx + 1).collect();

    // Apply aliases as substitutions in the body
    if !aliases.is_empty() {
        for stmt in &mut result {
            apply_hoisted_aliases_to_stmt(stmt, &aliases);
        }
    }

    result
}

fn apply_hoisted_aliases_to_stmt(stmt: &mut Statement, aliases: &BTreeMap<String, Expression>) {
    match stmt {
        Statement::Expr(e) => apply_hoisted_aliases_to_expr(e, aliases),
        Statement::Assign { target, value } => {
            apply_hoisted_aliases_to_target(target, aliases);
            apply_hoisted_aliases_to_expr(value, aliases);
        }
        Statement::Let { value, .. } => apply_hoisted_aliases_to_expr(value, aliases),
        Statement::Return(Some(e)) | Statement::Throw(e) => apply_hoisted_aliases_to_expr(e, aliases),
        Statement::If { condition, then_body, else_body } => {
            apply_hoisted_aliases_to_expr(condition, aliases);
            for s in then_body.iter_mut() { apply_hoisted_aliases_to_stmt(s, aliases); }
            for s in else_body.iter_mut() { apply_hoisted_aliases_to_stmt(s, aliases); }
        }
        Statement::While { condition, body } => {
            apply_hoisted_aliases_to_expr(condition, aliases);
            for s in body.iter_mut() { apply_hoisted_aliases_to_stmt(s, aliases); }
        }
        Statement::For { init, condition, update, body } => {
            if let Some(s) = init { apply_hoisted_aliases_to_stmt(s, aliases); }
            if let Some(e) = condition { apply_hoisted_aliases_to_expr(e, aliases); }
            if let Some(s) = update { apply_hoisted_aliases_to_stmt(s, aliases); }
            for s in body.iter_mut() { apply_hoisted_aliases_to_stmt(s, aliases); }
        }
        Statement::ForIn { object, body, .. } => {
            apply_hoisted_aliases_to_expr(object, aliases);
            for s in body.iter_mut() { apply_hoisted_aliases_to_stmt(s, aliases); }
        }
        Statement::ForOf { iterable, body, .. } => {
            apply_hoisted_aliases_to_expr(iterable, aliases);
            for s in body.iter_mut() { apply_hoisted_aliases_to_stmt(s, aliases); }
        }
        Statement::Switch { discriminant, cases, default } => {
            apply_hoisted_aliases_to_expr(discriminant, aliases);
            for (e, body) in cases.iter_mut() {
                apply_hoisted_aliases_to_expr(e, aliases);
                for s in body.iter_mut() { apply_hoisted_aliases_to_stmt(s, aliases); }
            }
            if let Some(body) = default { for s in body.iter_mut() { apply_hoisted_aliases_to_stmt(s, aliases); } }
        }
        Statement::TryCatch { try_body, catch_body, .. } => {
            for s in try_body.iter_mut() { apply_hoisted_aliases_to_stmt(s, aliases); }
            for s in catch_body.iter_mut() { apply_hoisted_aliases_to_stmt(s, aliases); }
        }
        _ => {}
    }
}

fn apply_hoisted_aliases_to_target(target: &mut AssignTarget, aliases: &BTreeMap<String, Expression>) {
    match target {
        AssignTarget::Member { object, .. } => apply_hoisted_aliases_to_expr(object, aliases),
        AssignTarget::Index { object, key } => {
            apply_hoisted_aliases_to_expr(object, aliases);
            apply_hoisted_aliases_to_expr(key, aliases);
        }
        _ => {}
    }
}

fn apply_hoisted_aliases_to_expr(expr: &mut Expression, aliases: &BTreeMap<String, Expression>) {
    match expr {
        Expression::Value(Value::Variable(name)) => {
            if let Some(replacement) = aliases.get(name.as_str()) {
                *expr = replacement.clone();
            }
        }
        Expression::Binary { left, right, .. } => {
            apply_hoisted_aliases_to_expr(left, aliases);
            apply_hoisted_aliases_to_expr(right, aliases);
        }
        Expression::Unary { operand, .. } => apply_hoisted_aliases_to_expr(operand, aliases),
        Expression::Call { callee, arguments } => {
            apply_hoisted_aliases_to_expr(callee, aliases);
            for a in arguments.iter_mut() { apply_hoisted_aliases_to_expr(a, aliases); }
        }
        Expression::New { callee, arguments } => {
            apply_hoisted_aliases_to_expr(callee, aliases);
            for a in arguments.iter_mut() { apply_hoisted_aliases_to_expr(a, aliases); }
        }
        Expression::Member { object, .. } => apply_hoisted_aliases_to_expr(object, aliases),
        Expression::Conditional { condition, then_expr, else_expr } => {
            apply_hoisted_aliases_to_expr(condition, aliases);
            apply_hoisted_aliases_to_expr(then_expr, aliases);
            apply_hoisted_aliases_to_expr(else_expr, aliases);
        }
        Expression::Array { elements } => {
            for item in elements.iter_mut().flatten() { apply_hoisted_aliases_to_expr(item, aliases); }
        }
        Expression::Object { properties } => {
            for prop in properties.iter_mut() { apply_hoisted_aliases_to_expr(&mut prop.value, aliases); }
        }
        Expression::Assignment { target, value } => {
            apply_hoisted_aliases_to_expr(target, aliases);
            apply_hoisted_aliases_to_expr(value, aliases);
        }
        Expression::Spread(inner) => apply_hoisted_aliases_to_expr(inner, aliases),
        _ => {}
    }
}
