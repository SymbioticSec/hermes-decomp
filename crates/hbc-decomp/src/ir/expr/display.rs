use super::{Expression, PropertyKey, BinaryOp};
use std::fmt;

impl fmt::Display for PropertyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", format_key(self))
    }
}

impl fmt::Display for Expression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", format_expr(self))
    }
}

pub fn format_key(key: &PropertyKey) -> String {
    match key {
        PropertyKey::Ident(name) => {
            if let Some(symbol_name) = name.strip_prefix("@@") {
                format!("[Symbol.{symbol_name}]")
            } else if name.chars().all(|c| c.is_ascii_digit()) {
                format!("\"{name}\"")
            } else {
                name.clone()
            }
        }
        PropertyKey::String(s) => {
            if let Some(symbol_name) = s.strip_prefix("@@") {
                format!("[Symbol.{symbol_name}]")
            } else {
                format!("\"{s}\"")
            }
        }
        PropertyKey::Computed(e) => format!("[{}]", format_expr(e)),
        PropertyKey::Index(i) => i.to_string(),
    }
}

// Format a member access expression with a customizable computed-key formatter.
// Display and Codegen share logic for Ident/String/Index; only Computed keys differ.
pub fn format_member_access_with(
    obj: &str, opt: &str, key: &PropertyKey,
    format_computed: impl Fn(&Expression) -> String,
) -> String {
    match key {
        PropertyKey::Ident(name) => {
            if let Some(symbol_name) = name.strip_prefix("@@") {
                format!("{obj}{opt}[Symbol.{symbol_name}]")
            } else if name.chars().all(|c| c.is_ascii_digit()) {
                format!("{obj}{opt}[{name}]")
            } else {
                format!("{obj}{opt}.{name}")
            }
        }
        PropertyKey::String(s) => {
            if let Some(symbol_name) = s.strip_prefix("@@") {
                format!("{obj}{opt}[Symbol.{symbol_name}]")
            } else {
                format!("{obj}{opt}[\"{s}\"]")
            }
        }
        PropertyKey::Computed(e) => format!("{obj}{opt}[{}]", format_computed(e)),
        PropertyKey::Index(i) => format!("{obj}{opt}[{i}]"),
    }
}

fn format_member_access(obj: &str, opt: &str, key: &PropertyKey) -> String {
    format_member_access_with(obj, opt, key, format_expr)
}

fn format_property(prop: &super::ObjectProperty) -> String {
    use crate::ir::Value;

    if let PropertyKey::Ident(key_name) = &prop.key {
        if let Expression::Value(Value::Variable(var_name)) = &prop.value {
            if key_name == var_name {
                return key_name.clone();
            }
        }
    }

    if let PropertyKey::Ident(key_name) = &prop.key {
        if let Expression::Function { name: Some(fn_name), .. } = &prop.value {
            if key_name == fn_name {
                return format_expr(&prop.value);
            }
        }
    }

    format!("{}: {}", format_key(&prop.key), format_expr(&prop.value))
}

fn format_expr_with_parens(expr: &Expression, parent_prec: u8) -> String {
    let needs_parens = match expr {
        Expression::Binary { op, .. } => op.precedence() < parent_prec,
        _ => false,
    };
    let s = format_expr(expr);
    if needs_parens { format!("({s})") } else { s }
}

fn join_exprs(exprs: &[Expression]) -> String {
    exprs.iter().map(format_expr).collect::<Vec<_>>().join(", ")
}

fn format_call(callee: &Expression, arguments: &[Expression], extra_suffix: &str) -> String {
    let callee_str = format_expr(callee);

    if let Some((first, rest)) = arguments.split_first() {
        let is_trivial_this = matches!(
            first,
            Expression::Value(crate::ir::Value::Constant(crate::ir::Constant::Undefined))
            | Expression::Value(crate::ir::Value::Global)
        ) || matches!(first, Expression::Value(crate::ir::Value::Variable(v)) if v == "globalThis");

        if is_trivial_this {
            format!("{}({}){}", callee_str, join_exprs(rest), extra_suffix)
        } else {
            let is_method_call = if let Expression::Member { object, .. } = callee {
                **object == *first || format_expr(object) == format_expr(first)
            } else {
                false
            };

            if is_method_call {
                format!("{}({}){}", callee_str, join_exprs(rest), extra_suffix)
            } else {
                format!("{}.call({}){}", callee_str, join_exprs(arguments), extra_suffix)
            }
        }
    } else {
        format!("{}({}){}", callee_str, join_exprs(arguments), extra_suffix)
    }
}

// Canonical expression formatter used by Display and Codegen.
pub fn format_expr(expr: &Expression) -> String {
    match expr {
        Expression::Value(v) => format!("{v}"),
        Expression::Binary { op, left, right } => {
            let prec = op.precedence();
            let (l_prec, r_prec) = if matches!(op, BinaryOp::Exp) {
                (prec + 1, prec)
            } else {
                (prec, prec + 1)
            };
            let l = format_expr_with_parens(left, l_prec);
            let r = format_expr_with_parens(right, r_prec);
            format!("{l} {op} {r}")
        }
        Expression::Unary { op, operand } => {
            format!("{op}{}", format_expr(operand))
        }
        Expression::Conditional { condition, then_expr, else_expr } => {
            format!("{} ? {} : {}",
                format_expr(condition),
                format_expr(then_expr),
                format_expr(else_expr)
            )
        }
        Expression::Member { object, property, optional } => {
            if !*optional {
                if let Expression::Value(crate::ir::Value::Global) = &**object {
                    if let PropertyKey::Ident(name) = property {
                        if is_builtin_global(name) {
                            return name.clone();
                        }
                    }
                }
                if let Expression::Value(crate::ir::Value::Variable(v)) = &**object {
                    if v == "globalThis" {
                        if let PropertyKey::Ident(name) = property {
                            if is_builtin_global(name) {
                                return name.clone();
                            }
                        }
                    }
                }
            }
            let obj = format_expr(object);
            let opt = if *optional { "?" } else { "" };
            if opt.is_empty() {
                if let PropertyKey::Ident(name) = property {
                    if obj == "globalThis" && is_builtin_global(name) {
                        return name.clone();
                    }
                }
            }
            format_member_access(&obj, opt, property)
        }
        Expression::Call { callee, arguments } => {
            if let Expression::Member { object, property: super::PropertyKey::Ident(method), .. } = callee.as_ref() {
                if method == "apply" && arguments.len() >= 3 {
                    let args_str = format_expr(&arguments[arguments.len() - 1]);
                    if args_str == "arguments" {
                        return format!("{}(...arguments)", format_expr(object));
                    }
                }
            }
            format_call(callee, arguments, "")
        }
        Expression::New { callee, arguments } => {
            format!("new {}({})", format_expr(callee), join_exprs(arguments))
        }
        Expression::Array { elements } => {
            let elems: Vec<String> = elements.iter()
                .map(|e| e.as_ref().map(format_expr).unwrap_or_default())
                .collect();
            format!("[{}]", elems.join(", "))
        }
        Expression::Object { properties } => {
            if properties.is_empty() {
                "{}".to_string()
            } else {
                let props: Vec<String> = properties.iter().map(format_property).collect();
                format!("{{ {} }}", props.join(", "))
            }
        }
        Expression::Function { id, name, is_arrow, is_async, is_generator } => {
            let async_prefix = if *is_async { "async " } else { "" };
            let gen_star = if *is_generator { "*" } else { "" };
            match (is_arrow, name) {
                (true, Some(n)) => format!("{async_prefix}function {n}() {{ ... }}"),
                (true, None) => format!("{async_prefix}() => {{ ... }}"),
                (false, Some(n)) => format!("{async_prefix}function{gen_star} {n}() {{ ... }}"),
                (false, None) => format!("/* F{} */ {}function{}() {{ ... }}", id.0, async_prefix, gen_star),
            }
        }
        Expression::Assignment { target, value } => {
            format!("{} = {}", format_expr(target), format_expr(value))
        }
        Expression::Spread(inner) => format!("...{}", format_expr(inner)),
        Expression::TemplateLiteral { quasis, expressions } => {
            let mut out = String::from("`");
            for (i, quasi) in quasis.iter().enumerate() {
                out.push_str(quasi);
                if let Some(expr) = expressions.get(i) {
                    out.push_str(&format!("${{{}}}", format_expr(expr)));
                }
            }
            out.push('`');
            out
        }
        Expression::RegExp { pattern, flags } => format!("/{pattern}/{flags}"),
        Expression::Yield { value, delegate } => {
            if *delegate {
                format!("yield* {}", format_expr(value))
            } else {
                format!("yield {}", format_expr(value))
            }
        }
        Expression::Await(value) => format!("await {}", format_expr(value)),
        Expression::JSXElement { tag, attributes, children } => {
            let mut attrs = Vec::new();
            for (key, val) in attributes {
                // If the value is a string constant without expressions, we could ideally render `key="value"`,
                // but for simplicity we render `key={value}` until refinement.
                attrs.push(format!("{}={{{}}}", key, format_expr(val)));
            }
            let attr_str = if attrs.is_empty() { String::new() } else { format!(" {}", attrs.join(" ")) };

            if children.is_empty() {
                format!("<{tag}{attr_str} />")
            } else {
                let child_str = children.iter().map(format_expr).collect::<Vec<_>>().join("");
                format!("<{tag}{attr_str}>{child_str}</{tag}>")
            }
        }
        Expression::Unknown { opcode, operands } => format!("/* {} {} */", opcode, operands.join(", ")),
    }
}

pub fn is_builtin_global(name: &str) -> bool {
    matches!(name,
        "Object" | "Array" | "Function" | "String" | "Number" | "Boolean" | "Symbol" |
        "Math" | "JSON" | "Date" | "RegExp" | "Promise" | "Proxy" | "Reflect" |
        "Map" | "Set" | "WeakMap" | "WeakSet" | "WeakRef" |
        "Error" | "TypeError" | "RangeError" | "ReferenceError" | "SyntaxError" | "URIError" | "EvalError" |
        "ArrayBuffer" | "SharedArrayBuffer" | "DataView" |
        "Int8Array" | "Uint8Array" | "Uint8ClampedArray" |
        "Int16Array" | "Uint16Array" | "Int32Array" | "Uint32Array" |
        "Float32Array" | "Float64Array" | "BigInt64Array" | "BigUint64Array" |
        "BigInt" | "Intl" | "Atomics" |
        "console" | "setTimeout" | "setInterval" | "clearTimeout" | "clearInterval" |
        "parseInt" | "parseFloat" | "isNaN" | "isFinite" |
        "encodeURI" | "decodeURI" | "encodeURIComponent" | "decodeURIComponent" |
        "NaN" | "Infinity" | "undefined" |
        "queueMicrotask" | "structuredClone" | "atob" | "btoa" |
        "fetch" | "Request" | "Response" | "Headers" | "URL" | "URLSearchParams" |
        "TextEncoder" | "TextDecoder" | "AbortController" | "AbortSignal" |
        "FormData" | "Blob" | "File" | "FileReader" |
        "performance" | "navigator" | "location" | "document" | "window" |
        "alert" | "confirm" | "prompt" |
        "HermesInternal" | "HermesBuiltin" | "__DEV__" | "ErrorUtils" | "__d" | "__r" |
        "requestAnimationFrame" | "cancelAnimationFrame" |
        "requestIdleCallback" | "cancelIdleCallback" |
        "setImmediate" | "clearImmediate" |
        "reportError" | "global" | "self" | "globalThis" |
        "process" | "Buffer" | "module" | "exports" | "require" |
        "WebSocket" | "XMLHttpRequest" | "Event" | "EventTarget" |
        "ReadableStream" | "WritableStream" | "TransformStream" |
        "DOMRect" | "DOMRectReadOnly" | "crypto" | "unescape" | "escape"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{BinaryOp, Constant, Value};

    #[test]
    fn test_precedence_parens() {
        let expr = Expression::binary(
            BinaryOp::Mul,
            Expression::binary(
                BinaryOp::Add,
                Expression::register(0),
                Expression::register(1),
            ),
            Expression::register(2),
        );
        assert_eq!(format!("{expr}"), "(r0 + r1) * r2");
    }
    #[test]
    fn test_format_expr_matches_display() {
        // Ensure format_expr and Display produce identical output
        let expr = Expression::binary(
            BinaryOp::Add,
            Expression::Value(Value::Register(0)),
            Expression::Value(Value::Constant(Constant::Integer(42))),
        );
        assert_eq!(format_expr(&expr), format!("{expr}"));
    }

    #[test]
    fn test_exp_right_associativity() {
        // a ** b ** c should NOT add parens (right-associative, natural grouping)
        let expr = Expression::binary(
            BinaryOp::Exp,
            Expression::register(0),
            Expression::binary(
                BinaryOp::Exp,
                Expression::register(1),
                Expression::register(2),
            ),
        );
        assert_eq!(format!("{expr}"), "r0 ** r1 ** r2");

        // (a ** b) ** c MUST add parens on left (overrides right-associativity)
        let expr = Expression::binary(
            BinaryOp::Exp,
            Expression::binary(
                BinaryOp::Exp,
                Expression::register(0),
                Expression::register(1),
            ),
            Expression::register(2),
        );
        assert_eq!(format!("{expr}"), "(r0 ** r1) ** r2");
    }
}
