// Definition-site closure naming, infers names from what value is assigned to closure_N.

use super::closure_usage::{ident_from_property, is_closure_name};
use crate::ir::{AssignTarget, Constant, Expression, PropertyKey, Statement, Value};
use std::collections::BTreeMap;

use super::closure_definitions::{collect_existing_names, make_unique_name};

// Rename closure variables based on their initialization expression (definition-site naming).
// Complements the usage-based `rename_closure_variables_cross_function` by looking
// at what value is assigned to `closure_N` in the function body.
//
// Returns the total number of variables renamed across all functions.
pub fn rename_closures_from_definitions(
    all_ir: &mut BTreeMap<u32, Vec<Statement>>,
) -> usize {
    let mut total = 0;
    let mut keys: Vec<_> = all_ir.keys().copied().collect();
    keys.sort();
    for key in keys {
        if let Some(stmts) = all_ir.get_mut(&key) {
            total += rename_closures_from_definitions_single(stmts);
        }
    }
    total
}

fn rename_closures_from_definitions_single(stmts: &mut [Statement]) -> usize {
    use std::collections::HashSet;

    // Phase 1: Collect existing names to avoid collisions
    let mut used_names: HashSet<String> = HashSet::new();
    collect_existing_names(stmts, &mut used_names);

    // Phase 2: Scan for closure_N definitions and infer names
    let mut renames: BTreeMap<String, String> = BTreeMap::new();
    scan_closure_defs(stmts, &mut renames, &mut used_names);

    if renames.is_empty() {
        return 0;
    }

    let count = renames.len();
    crate::analysis::naming::rename_variables_in_stmts(stmts, &renames);
    count
}

fn scan_closure_defs(
    stmts: &[Statement],
    renames: &mut BTreeMap<String, String>,
    used_names: &mut std::collections::HashSet<String>,
) {
    for stmt in stmts {
        scan_closure_def_in_stmt(stmt, renames, used_names);
    }
}

fn scan_closure_def_in_stmt(
    stmt: &Statement,
    renames: &mut BTreeMap<String, String>,
    used_names: &mut std::collections::HashSet<String>,
) {
    match stmt {
        Statement::Assign { target, value } => {
            if let AssignTarget::Variable(name) = target {
                try_infer_closure_def(name, value, renames, used_names);
            }
        }
        Statement::Let { name, value, .. } => {
            try_infer_closure_def(name, value, renames, used_names);
        }
        Statement::If { then_body, else_body, .. } => {
            scan_closure_defs(then_body, renames, used_names);
            scan_closure_defs(else_body, renames, used_names);
        }
        Statement::While { body, .. }
        | Statement::DoWhile { body, .. }
        | Statement::For { body, .. }
        | Statement::ForIn { body, .. }
        | Statement::ForOf { body, .. } => {
            scan_closure_defs(body, renames, used_names);
        }
        Statement::Block(inner) => {
            scan_closure_defs(inner, renames, used_names);
        }
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
            scan_closure_defs(try_body, renames, used_names);
            scan_closure_defs(catch_body, renames, used_names);
            scan_closure_defs(finally_body, renames, used_names);
        }
        Statement::Switch { cases, default, .. } => {
            for (_, body) in cases {
                scan_closure_defs(body, renames, used_names);
            }
            if let Some(d) = default {
                scan_closure_defs(d, renames, used_names);
            }
        }
        _ => {}
    }
}

fn try_infer_closure_def(
    name: &str,
    value: &Expression,
    renames: &mut BTreeMap<String, String>,
    used_names: &mut std::collections::HashSet<String>,
) {
    if !is_closure_name(name) || renames.contains_key(name) {
        return;
    }
    if let Some(inferred) = infer_name_from_definition(value) {
        let unique = make_unique_name(&inferred, used_names);
        renames.insert(name.to_string(), unique);
    }
}

// Infer a variable name from a definition expression.
fn infer_name_from_definition(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Call { callee, arguments } => {
            infer_from_call(callee, arguments)
        }
        Expression::Member { object, property, .. } => {
            infer_from_member(object, property)
        }
        Expression::New { callee, .. } => {
            infer_from_new_call(callee)
        }
        _ => None,
    }
}

// Infer name from a `new X()` constructor call.
fn infer_from_new_call(callee: &Expression) -> Option<String> {
    let name = match callee {
        Expression::Value(Value::Variable(n)) => n.as_str(),
        Expression::Member { property, .. } => {
            if let PropertyKey::Ident(n) = property { n.as_str() } else { return None; }
        }
        _ => return None,
    };
    match name {
        "Map" => Some("map".to_string()),
        "Set" => Some("set".to_string()),
        "WeakMap" => Some("weakMap".to_string()),
        "WeakSet" => Some("weakSet".to_string()),
        "WeakRef" => Some("weakRef".to_string()),
        "RegExp" => Some("pattern".to_string()),
        "AbortController" => Some("abortController".to_string()),
        "Promise" => Some("promise".to_string()),
        "XMLHttpRequest" => Some("xhr".to_string()),
        "EventEmitter" => Some("emitter".to_string()),
        _ => None,
    }
}

// Infer name from a function call expression.
fn infer_from_call(callee: &Expression, arguments: &[Expression]) -> Option<String> {
    // Symbol("name") → nameSymbol
    let is_symbol = match callee {
        Expression::Value(Value::Variable(n)) => n == "Symbol",
        Expression::Member { object, property, .. } => {
            matches!(&**object, Expression::Value(Value::Variable(n)) if n == "Symbol")
                && matches!(property, PropertyKey::Ident(p) if p == "for")
        }
        _ => false,
    };
    if is_symbol {
        if let Some(name) = arguments.first().and_then(extract_string_value) {
            let sanitized = super::suggestions::sanitize_name(&name);
            if !sanitized.is_empty() && sanitized.len() <= 30 {
                return Some(format!("{sanitized}Symbol"));
            }
        }
        return Some("sym".to_string());
    }

    // Method calls: X.method(args)
    if let Expression::Member { object, property, .. } = callee {
        if let Some(method) = ident_from_property(property) {
            if method == "createContext" {
                return Some("context".to_string());
            }
            if method == "create" {
                if let Expression::Value(Value::Variable(obj_name)) = &**object {
                    if obj_name.contains("StyleSheet") {
                        return Some("styles".to_string());
                    }
                }
            }
            if method == "default" {
                if let Expression::Value(Value::Variable(obj_name)) = &**object {
                    if obj_name.contains("PrivateField") || obj_name.contains("privateField") {
                        if let Some(field_name) = arguments.first().and_then(extract_string_value) {
                            let sanitized = super::suggestions::sanitize_name(&field_name);
                            if !sanitized.is_empty() && sanitized.len() <= 20 {
                                return Some(format!("{sanitized}Field"));
                            }
                        }
                        return Some("field".to_string());
                    }
                }
            }
            if method.len() > 3 && method.starts_with("get") && method.as_bytes()[3].is_ascii_uppercase() {
                return Some(to_camel_case(&method[3..]));
            }
            if method.len() > 3 && method.starts_with("set") && method.as_bytes()[3].is_ascii_uppercase() {
                return Some(to_camel_case(&method[3..]));
            }
        }
    }

    None
}

// Infer name from a member access expression.
fn infer_from_member(object: &Expression, property: &PropertyKey) -> Option<String> {
    let prop = ident_from_property(property)?;

    if matches!(prop.as_str(), "prototype" | "exports" | "__esModule" | "__proto__"
        | "constructor" | "length" | "toString" | "valueOf") {
        return None;
    }

    if prop == "default" {
        if let Expression::Value(Value::Variable(obj_name)) = object {
            if !is_closure_name(obj_name) && !is_generic_var_name(obj_name) && obj_name.len() <= 25 {
                return Some(obj_name.clone());
            }
        }
        return None;
    }

    let sanitized = super::suggestions::sanitize_name(&prop);
    if !sanitized.is_empty() && sanitized.len() <= 20 {
        return Some(sanitized);
    }

    None
}

fn extract_string_value(expr: &Expression) -> Option<String> {
    if let Expression::Value(Value::Constant(Constant::String(s))) = expr {
        Some(s.clone())
    } else {
        None
    }
}

fn is_generic_var_name(name: &str) -> bool {
    if name.starts_with("arg") && name[3..].chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    if name.starts_with("tmp") {
        return true;
    }
    if name.starts_with('r') && name.len() > 1 && name[1..].chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    matches!(name, "obj" | "val" | "fn" | "mod" | "lib" | "callback"
        | "arr" | "result" | "undefined" | "null" | "self")
}

// Convert PascalCase to camelCase.
fn to_camel_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => {
            let lower: String = c.to_lowercase().collect();
            format!("{}{}", lower, chars.collect::<String>())
        }
        None => String::new(),
    }
}
