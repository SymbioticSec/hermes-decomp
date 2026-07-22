// Infer parameter names from `throw new Error("…")` / similar messages.
// e.g. `throw new Error("Invalid email")` near uses of arg0 → hint "email".

use crate::ir::{Expression, Statement, Value};
use std::collections::BTreeMap;

/// Scan a function body for error-string hints keyed by parameter index
/// (when the message clearly references a single identifier-like word that
/// matches a nearby parameter access, conservative: extract candidate words
/// from error strings and associate with any Parameter used in the same
/// throw/call expression tree).
pub fn hints_from_error_strings(stmts: &[Statement]) -> BTreeMap<u32, Vec<String>> {
    let mut out: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for stmt in stmts {
        walk_stmt(stmt, &mut out);
    }
    out
}

fn walk_stmt(stmt: &Statement, out: &mut BTreeMap<u32, Vec<String>>) {
    match stmt {
        Statement::Throw(e) => collect_from_throw_expr(e, out),
        Statement::Expr(e) => {
            if is_error_call(e) {
                collect_from_throw_expr(e, out);
            }
        }
        Statement::If {
            then_body,
            else_body,
            ..
        } => {
            for s in then_body {
                walk_stmt(s, out);
            }
            for s in else_body {
                walk_stmt(s, out);
            }
        }
        Statement::While { body, .. }
        | Statement::DoWhile { body, .. }
        | Statement::For { body, .. }
        | Statement::ForIn { body, .. }
        | Statement::ForOf { body, .. }
        | Statement::Block(body) => {
            for s in body {
                walk_stmt(s, out);
            }
        }
        Statement::TryCatch {
            try_body,
            catch_body,
            finally_body,
            ..
        } => {
            for s in try_body {
                walk_stmt(s, out);
            }
            for s in catch_body {
                walk_stmt(s, out);
            }
            for s in finally_body {
                walk_stmt(s, out);
            }
        }
        _ => {}
    }
}

fn is_error_call(expr: &Expression) -> bool {
    matches!(
        expr,
        Expression::New { callee, .. } if is_error_ctor(callee)
    ) || matches!(
        expr,
        Expression::Call { callee, .. } if is_error_ctor(callee)
    )
}

fn is_error_ctor(expr: &Expression) -> bool {
    match expr {
        Expression::Value(Value::Variable(n)) => {
            n == "Error"
                || n == "TypeError"
                || n == "RangeError"
                || n == "SyntaxError"
                || n.ends_with("Error")
        }
        Expression::Member {
            property: crate::ir::PropertyKey::Ident(p) | crate::ir::PropertyKey::String(p),
            ..
        } => p.ends_with("Error"),
        _ => false,
    }
}

fn collect_from_throw_expr(expr: &Expression, out: &mut BTreeMap<u32, Vec<String>>) {
    let mut params = Vec::new();
    let mut words = Vec::new();
    collect_params_and_strings(expr, &mut params, &mut words);
    if params.is_empty() || words.is_empty() {
        // Still try: error message alone, single param in enclosing... skip if no param in tree
        return;
    }
    for word in words {
        for &p in &params {
            out.entry(p).or_default().push(word.clone());
        }
    }
}

fn collect_params_and_strings(
    expr: &Expression,
    params: &mut Vec<u32>,
    words: &mut Vec<String>,
) {
    match expr {
        Expression::Value(Value::Parameter(i)) => params.push(*i),
        Expression::Value(Value::Constant(crate::ir::Constant::String(s))) => {
            for w in extract_name_words(s) {
                words.push(w);
            }
        }
        Expression::Binary { left, right, .. } => {
            collect_params_and_strings(left, params, words);
            collect_params_and_strings(right, params, words);
        }
        Expression::Unary { operand, .. }
        | Expression::Spread(operand)
        | Expression::Await(operand)
        | Expression::Yield { value: operand, .. } => {
            collect_params_and_strings(operand, params, words);
        }
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            collect_params_and_strings(callee, params, words);
            for a in arguments {
                collect_params_and_strings(a, params, words);
            }
        }
        Expression::Member { object, .. } => collect_params_and_strings(object, params, words),
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_params_and_strings(condition, params, words);
            collect_params_and_strings(then_expr, params, words);
            collect_params_and_strings(else_expr, params, words);
        }
        Expression::TemplateLiteral { expressions, quasis } => {
            for e in expressions {
                collect_params_and_strings(e, params, words);
            }
            for q in quasis {
                for w in extract_name_words(q) {
                    words.push(w);
                }
            }
        }
        _ => {}
    }
}

fn extract_name_words(s: &str) -> Vec<String> {
    // Prefer last identifier-like token: "Invalid email" → email, "expected userId" → userId
    let mut best = Vec::new();
    for token in s.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
        if token.len() < 2 || token.len() > 32 {
            continue;
        }
        let mut chars = token.chars();
        let Some(first) = chars.next() else { continue };
        if !first.is_ascii_lowercase() {
            continue; // skip Invalid, Error, MUST_...
        }
        if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        // Reject noise
        if matches!(
            token,
            "to" | "be" | "is" | "of" | "in" | "at" | "an" | "or" | "and" | "not" | "the"
                | "for" | "with" | "from" | "this" | "that" | "must" | "should" | "cannot"
                | "invalid" | "expected" | "missing" | "required" | "undefined" | "null"
        ) {
            continue;
        }
        best.push(token.to_string());
    }
    // Keep last 2 candidates max
    if best.len() > 2 {
        best = best.split_off(best.len() - 2);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Constant;

    #[test]
    fn extracts_email_from_error() {
        let throw = Statement::Throw(Expression::New {
            callee: Box::new(Expression::Value(Value::Variable("Error".into()))),
            arguments: vec![
                Expression::Value(Value::Constant(Constant::String("Invalid email".into()))),
                Expression::Value(Value::Parameter(0)),
            ],
        });
        // Parameter may not be in Error() args often, put param in message build
        let throw2 = Statement::Throw(Expression::New {
            callee: Box::new(Expression::Value(Value::Variable("TypeError".into()))),
            arguments: vec![Expression::Binary {
                op: crate::ir::BinaryOp::Add,
                left: Box::new(Expression::Value(Value::Constant(Constant::String(
                    "bad email: ".into(),
                )))),
                right: Box::new(Expression::Value(Value::Parameter(0))),
            }],
        });
        let hints = hints_from_error_strings(&[throw, throw2]);
        assert!(hints.get(&0).is_some_and(|v| v.iter().any(|s| s == "email")));
    }
}
