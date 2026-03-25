// Strip meaningless Hermes `this` arguments from calls.
//
// In Hermes bytecode, ALL function calls pass `this` as arguments[0].
// For non-method calls, this is meaningless (register reuse artifact).
// This pass strips it at the IR level so that dead assignment elimination
// can catch variables that were only used as `this` arguments.

use crate::ir::{AssignTarget, Expression, PropertyKey, Statement};

// Strip meaningless `this` from Call expressions at the IR level.
// - For method calls (callee is Member, object == args[0]): strip args[0]
// - For variable callees (not Member): strip args[0] if non-trivial
// - For explicit .call (callee is Member with prop "call"): strip args[0] (function self-ref)
// - Keep args[0] only when it's needed (already undefined/global -> keep as-is for consistency)
pub fn strip_hermes_this(stmts: &mut [Statement]) {
    for stmt in stmts.iter_mut() {
        strip_this_in_stmt(stmt);
    }
}

fn strip_this_in_stmt(stmt: &mut Statement) {
    match stmt {
        Statement::Assign { target, value } => {
            strip_this_in_target(target);
            strip_this_in_expr(value);
        }
        Statement::Let { value, .. } => strip_this_in_expr(value),
        Statement::Expr(e) => strip_this_in_expr(e),
        Statement::Return(Some(e)) | Statement::Throw(e) => strip_this_in_expr(e),
        Statement::If { condition, then_body, else_body } => {
            strip_this_in_expr(condition);
            strip_hermes_this(then_body);
            strip_hermes_this(else_body);
        }
        Statement::While { condition, body } | Statement::DoWhile { condition, body } => {
            strip_this_in_expr(condition);
            strip_hermes_this(body);
        }
        Statement::For { init, condition, update, body } => {
            if let Some(s) = init { strip_this_in_stmt(s); }
            if let Some(e) = condition { strip_this_in_expr(e); }
            if let Some(s) = update { strip_this_in_stmt(s); }
            strip_hermes_this(body);
        }
        Statement::ForIn { object, body, .. } => {
            strip_this_in_expr(object);
            strip_hermes_this(body);
        }
        Statement::ForOf { iterable, body, .. } => {
            strip_this_in_expr(iterable);
            strip_hermes_this(body);
        }
        Statement::Block(inner) => strip_hermes_this(inner),
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
            strip_hermes_this(try_body);
            strip_hermes_this(catch_body);
            strip_hermes_this(finally_body);
        }
        Statement::Switch { discriminant, cases, default } => {
            strip_this_in_expr(discriminant);
            for (e, body) in cases.iter_mut() {
                strip_this_in_expr(e);
                strip_hermes_this(body);
            }
            if let Some(d) = default { strip_hermes_this(d); }
        }
        _ => {}
    }
}

fn strip_this_in_target(target: &mut AssignTarget) {
    match target {
        AssignTarget::Member { object, .. } => strip_this_in_expr(object),
        AssignTarget::Index { object, key } => {
            strip_this_in_expr(object);
            strip_this_in_expr(key);
        }
        _ => {}
    }
}

fn strip_this_in_expr(expr: &mut Expression) {
    match expr {
        Expression::Call { callee, arguments } => {
            // First recurse into sub-expressions
            strip_this_in_expr(callee);
            for arg in arguments.iter_mut() {
                strip_this_in_expr(arg);
            }
            // Now strip `this` (arguments[0]) if appropriate.
            // In Hermes bytecode, ALL function calls pass `this` as arguments[0].
            if arguments.is_empty() {
                return;
            }
            let first = &arguments[0];
            let should_strip = if let Expression::Member { object, property, .. } = callee.as_ref() {
                if matches!(property, PropertyKey::Ident(p) if p == "call") {
                    // Explicit .call() from source: strip the function self-reference (first arg),
                    // keeping the user-supplied receiver as the new first arg.
                    true
                } else {
                    // Member callee (method call): strip if object matches first arg (receiver IS the object)
                    // For non-matching (e.g., globalThis.func(garbage_this, ...)), also strip.
                    let is_method_call = **object == *first;
                    if is_method_call {
                        true // Standard method call: obj.method(obj, args) -> obj.method(args)
                    } else {
                        // Member on global or unrelated receiver -- always strip (garbage this)
                        true
                    }
                }
            } else {
                // Variable callee or trivial this (undefined/global) -- always strip
                true
            };
            if should_strip {
                arguments.remove(0);
            }
        }
        Expression::Binary { left, right, .. } => {
            strip_this_in_expr(left);
            strip_this_in_expr(right);
        }
        Expression::Unary { operand, .. } => strip_this_in_expr(operand),
        Expression::New { callee, arguments } => {
            strip_this_in_expr(callee);
            for a in arguments.iter_mut() { strip_this_in_expr(a); }
        }
        Expression::Member { object, .. } => strip_this_in_expr(object),
        Expression::Conditional { condition, then_expr, else_expr } => {
            strip_this_in_expr(condition);
            strip_this_in_expr(then_expr);
            strip_this_in_expr(else_expr);
        }
        Expression::Array { elements } => {
            for e in elements.iter_mut().flatten() { strip_this_in_expr(e); }
        }
        Expression::Object { properties } => {
            for p in properties.iter_mut() { strip_this_in_expr(&mut p.value); }
        }
        Expression::Assignment { target, value } => {
            strip_this_in_expr(target);
            strip_this_in_expr(value);
        }
        Expression::Spread(inner) => strip_this_in_expr(inner),
        Expression::TemplateLiteral { expressions, .. } => {
            for e in expressions.iter_mut() { strip_this_in_expr(e); }
        }
        Expression::Yield { value, .. } => strip_this_in_expr(value),
        Expression::Await(inner) => strip_this_in_expr(inner),
        _ => {}
    }
}
