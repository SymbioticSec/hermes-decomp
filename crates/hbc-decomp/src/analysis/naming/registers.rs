use crate::ir::{AssignTarget, Constant, Expression, PropertyKey, Statement, Value};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Default)]
pub struct RegisterInfo {
    pub role: RegisterRole,
    pub accessed_props: HashSet<String>,
    pub called_methods: HashSet<String>,
    pub from_param: Option<u32>,
    pub from_property: Option<String>,
    // If assigned via destructuring: the property key name
    pub destructuring_key: Option<String>,
    // If assigned a named function value, its own name (e.g. a Babel/Metro helper
    // `_typeof`, `_interopRequireDefault`, …) — used verbatim instead of "fn".
    pub function_name: Option<String>,
    pub use_count: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum RegisterRole {
    #[default]
    Unknown,
    Array,
    Object,
    Function,
    String,
    Number,
    Boolean,
    BigInt,
    Iterator,
    Promise,
    This,
    Undefined,
    Null,
}

pub fn analyze_registers(stmts: &[Statement]) -> BTreeMap<u32, RegisterInfo> {
    let mut info: BTreeMap<u32, RegisterInfo> = BTreeMap::new();

    for stmt in stmts {
        analyze_stmt(stmt, &mut info);
    }

    info
}

fn analyze_stmt(stmt: &Statement, info: &mut BTreeMap<u32, RegisterInfo>) {
    match stmt {
        Statement::Assign { target, value } => {
            if let AssignTarget::Register(r) = target {
                let entry = info.entry(*r).or_default();
                infer_role_from_value(value, entry);
            }
            analyze_expr(value, info);
            analyze_target(target, info);
        }
        Statement::Expr(e) => analyze_expr(e, info),
        Statement::Return(Some(e)) => analyze_expr(e, info),
        Statement::Throw(e) => analyze_expr(e, info),
        Statement::If {
            condition,
            then_body,
            else_body,
        } => {
            analyze_expr(condition, info);
            for s in then_body {
                analyze_stmt(s, info);
            }
            for s in else_body {
                analyze_stmt(s, info);
            }
        }
        Statement::While { condition, body } => {
            analyze_expr(condition, info);
            for s in body {
                analyze_stmt(s, info);
            }
        }
        Statement::Block(inner) => {
            for s in inner {
                analyze_stmt(s, info);
            }
        }
        Statement::Let { value, .. } => {
            analyze_expr(value, info);
        }
        Statement::For { init, condition, update, body } => {
            if let Some(i) = init {
                analyze_stmt(i, info);
            }
            if let Some(c) = condition {
                analyze_expr(c, info);
            }
            if let Some(u) = update {
                analyze_stmt(u, info);
            }
            for s in body {
                analyze_stmt(s, info);
            }
        }
        Statement::DoWhile { body, condition } => {
            for s in body {
                analyze_stmt(s, info);
            }
            analyze_expr(condition, info);
        }
        Statement::ForIn { object, body, .. } => {
            analyze_expr(object, info);
            for s in body {
                analyze_stmt(s, info);
            }
        }
        Statement::ForOf { iterable, body, .. } => {
            analyze_expr(iterable, info);
            for s in body {
                analyze_stmt(s, info);
            }
        }
        Statement::Switch { discriminant, cases, default } => {
            analyze_expr(discriminant, info);
            for (val, body) in cases {
                analyze_expr(val, info);
                for s in body {
                    analyze_stmt(s, info);
                }
            }
            if let Some(d) = default {
                for s in d {
                    analyze_stmt(s, info);
                }
            }
        }
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
            for s in try_body {
                analyze_stmt(s, info);
            }
            for s in catch_body {
                analyze_stmt(s, info);
            }
            for s in finally_body {
                analyze_stmt(s, info);
            }
        }
        _ => {}
    }
}

fn analyze_target(target: &AssignTarget, info: &mut BTreeMap<u32, RegisterInfo>) {
    match target {
        AssignTarget::Member { object, .. } => analyze_expr(object, info),
        AssignTarget::Index { object, key } => {
            analyze_expr(object, info);
            analyze_expr(key, info);
        }
        AssignTarget::DestructuringObject(props) => {
            for (key, t, def) in props {
                // Name register after its destructuring key
                if let AssignTarget::Register(r) = t {
                    let entry = info.entry(*r).or_default();
                    entry.destructuring_key = Some(key.clone());
                }
                analyze_target(t, info);
                if let Some(d) = def { analyze_expr(d, info); }
            }
        }
        AssignTarget::DestructuringObjectRest { properties, rest } => {
            for (key, t, def) in properties {
                if let AssignTarget::Register(r) = t {
                    let entry = info.entry(*r).or_default();
                    entry.destructuring_key = Some(key.clone());
                }
                analyze_target(t, info);
                if let Some(d) = def { analyze_expr(d, info); }
            }
            analyze_target(rest, info);
        }
        AssignTarget::DestructuringArray(elements) => {
            for elem in elements.iter().flatten() {
                analyze_target(&elem.0, info);
                if let Some(d) = &elem.1 { analyze_expr(d, info); }
            }
        }
        AssignTarget::DestructuringArrayRest { elements, rest } => {
            for elem in elements.iter().flatten() {
                analyze_target(&elem.0, info);
                if let Some(d) = &elem.1 { analyze_expr(d, info); }
            }
            analyze_target(rest, info);
        }
        _ => {}
    }
}

fn analyze_expr(expr: &Expression, info: &mut BTreeMap<u32, RegisterInfo>) {
    match expr {
        Expression::Value(Value::Register(r)) => {
            info.entry(*r).or_default().use_count += 1;
        }
        Expression::Member {
            object, property, ..
        } => {
            // Track property access
            if let Expression::Value(Value::Register(r)) = object.as_ref() {
                let entry = info.entry(*r).or_default();
                if let PropertyKey::Ident(name) = property {
                    entry.accessed_props.insert(name.clone());
                    // Infer type from property
                    infer_role_from_property(name, entry);
                }
            }
            analyze_expr(object, info);
            if let PropertyKey::Computed(k) = property {
                analyze_expr(k, info);
            }
        }
        Expression::Call { callee, arguments } => {
            // Track method calls
            if let Expression::Member {
                object,
                property: PropertyKey::Ident(method),
                ..
            } = callee.as_ref()
            {
                if let Expression::Value(Value::Register(r)) = object.as_ref() {
                    info.entry(*r)
                        .or_default()
                        .called_methods
                        .insert(method.clone());
                }
            }
            analyze_expr(callee, info);
            for arg in arguments {
                analyze_expr(arg, info);
            }
        }
        Expression::Binary { left, right, .. } => {
            analyze_expr(left, info);
            analyze_expr(right, info);
        }
        Expression::Unary { operand, .. } => analyze_expr(operand, info),
        Expression::New { callee, arguments } => {
            analyze_expr(callee, info);
            for arg in arguments {
                analyze_expr(arg, info);
            }
        }
        Expression::Array { elements } => {
            for elem in elements.iter().flatten() {
                analyze_expr(elem, info);
            }
        }
        Expression::Object { properties } => {
            for prop in properties {
                analyze_expr(&prop.value, info);
            }
        }
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            analyze_expr(condition, info);
            analyze_expr(then_expr, info);
            analyze_expr(else_expr, info);
        }
        Expression::Assignment { target, value } => {
            analyze_expr(target, info);
            analyze_expr(value, info);
        }
        Expression::Spread(inner) => analyze_expr(inner, info),
        Expression::TemplateLiteral { expressions, .. } => {
            for e in expressions {
                analyze_expr(e, info);
            }
        }
        Expression::Yield { value, .. } => analyze_expr(value, info),
        Expression::Await(inner) => analyze_expr(inner, info),
        _ => {}
    }
}

// A function's own name worth adopting as a variable name: a valid JS identifier
// (this rejects Hermes markers like `<anonymous>`), at least two characters, and
// not a decompiler placeholder (`f1234`).
fn is_meaningful_fn_name(name: &str) -> bool {
    if name.len() < 2 {
        return false;
    }
    // Must be a valid identifier: first char a letter/`_`/`$`, rest alphanumerics.
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {}
        _ => return false,
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') {
        return false;
    }
    // `f` followed by only digits is a synthetic placeholder.
    if let Some(rest) = name.strip_prefix('f') {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
    }
    true
}

fn infer_role_from_value(value: &Expression, info: &mut RegisterInfo) {
    match value {
        Expression::Array { .. } => info.role = RegisterRole::Array,
        Expression::Object { .. } => info.role = RegisterRole::Object,
        Expression::Function { name, .. } => {
            info.role = RegisterRole::Function;
            if let Some(fname) = name {
                if is_meaningful_fn_name(fname) {
                    info.function_name = Some(fname.clone());
                }
            }
        }
        Expression::Value(Value::Constant(c)) => {
            info.role = match c {
                Constant::String(_) => RegisterRole::String,
                Constant::Integer(_) | Constant::Number(_) => RegisterRole::Number,
                Constant::Bool(_) => RegisterRole::Boolean,
                Constant::BigInt(_) => RegisterRole::BigInt,
                Constant::Null => RegisterRole::Null,
                Constant::Undefined => RegisterRole::Undefined,
            };
        }
        Expression::Value(Value::This) => info.role = RegisterRole::This,
        Expression::Member {
            property: PropertyKey::Ident(name),
            ..
        } => {
            info.from_property = Some(name.clone());
        }
        _ => {}
    }
}

fn infer_role_from_property(prop: &str, info: &mut RegisterInfo) {
    match prop {
        "length" | "push" | "pop" | "shift" | "unshift" | "splice" | "slice" | "map" | "filter"
        | "reduce" | "forEach" | "find" | "indexOf" => {
            if info.role == RegisterRole::Unknown {
                info.role = RegisterRole::Array;
            }
        }
        "then" | "catch" | "finally" => {
            if info.role == RegisterRole::Unknown {
                info.role = RegisterRole::Promise;
            }
        }
        "next" | "done" | "value" => {
            if info.role == RegisterRole::Unknown {
                info.role = RegisterRole::Iterator;
            }
        }
        "toString" | "charAt" | "substring" | "substr" | "split" | "trim" | "toLowerCase"
        | "toUpperCase" | "replace" | "match" => {
            if info.role == RegisterRole::Unknown {
                info.role = RegisterRole::String;
            }
        }
        _ => {}
    }
}
