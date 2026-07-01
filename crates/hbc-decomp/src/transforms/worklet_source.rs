// Recover original worklet sources embedded by the Reanimated Babel plugin.
//
// For every worklet, the plugin emits an `__initData` object literal whose
// `code` property is the ORIGINAL source string, e.g.
//   { code: "function fooWorklet(x){const{a,b}=this.__closure; ...}", location, sourceMap }
// Hermes compiles that string as an ordinary string constant, so it survives
// verbatim in the `.hbc` string table — names and all. The compiled worklet
// carries the same function name in the bytecode name table, so we can join the
// recovered source to its function by name and emit the real source instead of
// our (lossy, sometimes mis-structured) decompilation. Pure blackbox: both the
// string and the name come from the binary.

use crate::ir::{Constant, Expression, PropertyKey, Statement, Value};
use std::collections::BTreeMap;

// function name -> original source (`function NAME(...){...}`)
pub fn collect_worklet_sources(all_ir: &BTreeMap<u32, Vec<Statement>>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for stmts in all_ir.values() {
        for stmt in stmts {
            collect_in_stmt(stmt, &mut out);
        }
    }
    out
}

fn collect_in_stmt(stmt: &Statement, out: &mut BTreeMap<String, String>) {
    // Any object literal with a `code: "function NAME(...){...}"` property.
    match stmt {
        Statement::Assign { value, .. } | Statement::Let { value, .. } | Statement::Expr(value) => {
            collect_in_expr(value, out)
        }
        _ => {}
    }
    // Recurse into nested bodies.
    match stmt {
        Statement::If { then_body, else_body, .. } => {
            then_body.iter().for_each(|s| collect_in_stmt(s, out));
            else_body.iter().for_each(|s| collect_in_stmt(s, out));
        }
        Statement::While { body, .. }
        | Statement::DoWhile { body, .. }
        | Statement::For { body, .. }
        | Statement::ForIn { body, .. }
        | Statement::ForOf { body, .. } => body.iter().for_each(|s| collect_in_stmt(s, out)),
        Statement::Block(inner) => inner.iter().for_each(|s| collect_in_stmt(s, out)),
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
            try_body.iter().for_each(|s| collect_in_stmt(s, out));
            catch_body.iter().for_each(|s| collect_in_stmt(s, out));
            finally_body.iter().for_each(|s| collect_in_stmt(s, out));
        }
        Statement::Switch { cases, default, .. } => {
            for (_, body) in cases {
                body.iter().for_each(|s| collect_in_stmt(s, out));
            }
            if let Some(d) = default {
                d.iter().for_each(|s| collect_in_stmt(s, out));
            }
        }
        _ => {}
    }
}

fn collect_in_expr(expr: &Expression, out: &mut BTreeMap<String, String>) {
    if let Expression::Object { properties } = expr {
        for p in properties {
            let is_code = matches!(&p.key, PropertyKey::Ident(k) | PropertyKey::String(k) if k == "code");
            if is_code {
                if let Expression::Value(Value::Constant(Constant::String(src))) = &p.value {
                    if let Some(name) = worklet_fn_name(src) {
                        out.insert(name, src.clone());
                    }
                }
            }
            collect_in_expr(&p.value, out);
        }
    }
    // Recurse into nested object/array values that might hold initData.
    match expr {
        Expression::Array { elements } => {
            for e in elements.iter().flatten() {
                collect_in_expr(e, out);
            }
        }
        Expression::Assignment { value, .. } => collect_in_expr(value, out),
        _ => {}
    }
}

// If `src` is a worklet source (`function NAME(...) {...}`), return NAME.
fn worklet_fn_name(src: &str) -> Option<String> {
    let rest = src.strip_prefix("function ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    // Must be followed by `(` and look like a real worklet (captures or a body).
    let after = &rest[name.len()..];
    if name.is_empty() || !after.starts_with('(') {
        return None;
    }
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_name() {
        assert_eq!(
            worklet_fn_name("function fooWorklet(x){const{a}=this.__closure;return a;}"),
            Some("fooWorklet".to_string())
        );
        assert_eq!(worklet_fn_name("function Abc123(){}"), Some("Abc123".to_string()));
        assert_eq!(worklet_fn_name("not a function"), None);
        assert_eq!(worklet_fn_name("function (){}"), None); // anonymous
    }

    #[test]
    fn collects_from_initdata_object() {
        let stmts = vec![Statement::Assign {
            target: crate::ir::AssignTarget::Variable("d".to_string()),
            value: Expression::Object {
                properties: vec![crate::ir::ObjectProperty {
                    key: PropertyKey::Ident("code".to_string()),
                    value: Expression::Value(Value::Constant(Constant::String(
                        "function w1(p){return p;}".to_string(),
                    ))),
                }],
            },
        }];
        let mut all = BTreeMap::new();
        all.insert(0u32, stmts);
        let map = collect_worklet_sources(&all);
        assert_eq!(map.get("w1").map(|s| s.as_str()), Some("function w1(p){return p;}"));
    }
}
