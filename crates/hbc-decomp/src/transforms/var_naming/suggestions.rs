use crate::ir::{Expression, PropertyKey, Value};

pub fn get_function_name(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Value(Value::Variable(name)) => Some(name.clone()),
        Expression::Member {
            property: PropertyKey::Ident(name),
            ..
        } => Some(name.clone()),
        _ => None,
    }
}

// Name for qualified calls like `StyleSheet.create()`, `Object.keys()`, etc.
pub fn name_for_qualified_call(qualified: &str) -> Option<String> {
    match qualified.to_lowercase().as_str() {
        "stylesheet.create" => Some("styles".to_string()),
        "stylesheet.flatten" => Some("flatStyles".to_string()),
        "object.keys" => Some("keys".to_string()),
        "object.values" => Some("values".to_string()),
        "object.entries" => Some("entries".to_string()),
        "object.assign" => Some("merged".to_string()),
        "object.create" => Some("obj".to_string()),
        "object.freeze" => Some("frozen".to_string()),
        "json.parse" => Some("parsed".to_string()),
        "json.stringify" => Some("json".to_string()),
        "array.from" => Some("arr".to_string()),
        "array.isarray" => Some("isArray".to_string()),
        "promise.all" => Some("allPromises".to_string()),
        "promise.race" => Some("racePromise".to_string()),
        "promise.resolve" => Some("resolved".to_string()),
        "date.now" => Some("timestamp".to_string()),
        "math.floor" | "math.ceil" | "math.round" => Some("rounded".to_string()),
        "math.max" | "math.min" => Some("bound".to_string()),
        "math.abs" => Some("absolute".to_string()),
        "math.random" => Some("random".to_string()),
        "regexp" => Some("regex".to_string()),
        _ => None,
    }
}

pub fn name_for_call(func_name: &str) -> String {
    match func_name.to_lowercase().as_str() {
        "fetch" => "response".to_string(),
        "json" => "data".to_string(),
        "parse" | "parseint" | "parsefloat" => "parsed".to_string(),
        "stringify" => "json".to_string(),
        "getitem" | "get" => "value".to_string(),
        "setitem" | "set" => "result".to_string(),
        "find" | "filter" => "found".to_string(),
        "map" => "mapped".to_string(),
        "reduce" => "reduced".to_string(),
        "split" => "parts".to_string(),
        "join" => "joined".to_string(),
        "slice" | "substring" | "substr" => "substr".to_string(),
        "tostring" => "str".to_string(),
        "tolowercase" | "touppercase" => "formatted".to_string(),
        "trim" => "trimmed".to_string(),
        "replace" | "replaceall" => "replaced".to_string(),
        "match" | "exec" => "match".to_string(),
        "test" => "isMatch".to_string(),
        "keys" => "keys".to_string(),
        "values" => "values".to_string(),
        "entries" => "entries".to_string(),
        "assign" | "create" => "obj".to_string(),
        "concat" => "combined".to_string(),
        "push" | "pop" | "shift" | "unshift" => "arr".to_string(),
        "sort" => "sorted".to_string(),
        "reverse" => "reversed".to_string(),
        "includes" | "has" | "contains" => "hasItem".to_string(),
        "indexof" => "index".to_string(),
        "foreach" => "item".to_string(),
        "settimeout" | "setinterval" => "timerId".to_string(),
        "mutate" => "mutation".to_string(), // Explicit Apollo pattern
        "query" => "query".to_string(),
        "subscribe" => "subscription".to_string(),
        "promise" => "promiseObj".to_string(),
        "then" => "nextPromise".to_string(),
        "catch" => "catchPromise".to_string(),
        "finally" => "cleanupPromise".to_string(),
        "require" => "module".to_string(),
        "createelement" => "element".to_string(),
        "getelementbyid" | "queryselector" => "element".to_string(),
        "queryselectorall" | "getelementsbytagname" | "getelementsbyclassname" => {
            "elements".to_string()
        }
        "addeventlistener" => "listener".to_string(),
        "removeeventlistener" => "removed".to_string(),
        "classlist" => "classes".to_string(),
        "style" => "style".to_string(),
        "getattribute" | "setattribute" => "attr".to_string(),
        _ => {
            // A factory/accessor/hook call names its result after what it makes:
            // `createUpdate()` → "update", `useSharedValue()` → "sharedValue",
            // `getUser()` → "user". Much more readable than `createUpdateResult`.
            if let Some(stripped) = strip_factory_prefix(func_name) {
                return stripped;
            }
            // Use function name as base if it's reasonable
            if func_name.len() <= 20 && func_name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                format!("{func_name}Result")
            } else {
                "result".to_string()
            }
        }
    }
}

// Strip a leading factory/accessor/hook verb (`create`, `make`, `get`, `use`,
// ...) and return the remainder with a lowercased first letter, when the result
// is a clean identifier. `createUpdate` → "update", `useState` → "state".
fn strip_factory_prefix(func_name: &str) -> Option<String> {
    const PREFIXES: &[&str] = &[
        "create", "make", "build", "get", "use", "fetch", "load", "request",
        "compute", "generate", "init", "resolve", "read", "select",
    ];
    for p in PREFIXES {
        if let Some(rest) = func_name.strip_prefix(p) {
            // The boundary must be a real word boundary: the remainder starts
            // uppercase (camelCase) and is a usable name on its own.
            let mut chars = rest.chars();
            match chars.next() {
                Some(first) if first.is_ascii_uppercase() && rest.len() >= 2 => {
                    if rest.chars().all(|c| c.is_alphanumeric()) {
                        let name = format!("{}{}", first.to_ascii_lowercase(), chars.as_str());
                        if !crate::constants::is_reserved_word(&name) {
                            return Some(name);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    None
}

pub fn name_for_property(prop: &str) -> String {
    match prop.to_lowercase().as_str() {
        "length" => "len".to_string(),
        "prototype" => "proto".to_string(),
        "constructor" => "ctor".to_string(),
        "name" => "name".to_string(),
        "value" => "val".to_string(),
        "data" => "data".to_string(),
        "result" | "results" => "result".to_string(),
        "error" | "errors" => "err".to_string(),
        "message" | "msg" => "msg".to_string(),
        "status" => "status".to_string(),
        "code" => "code".to_string(),
        "type" => "type".to_string(),
        "id" => "id".to_string(),
        "key" => "key".to_string(),
        "index" | "idx" => "idx".to_string(),
        "item" | "items" => "item".to_string(),
        "element" | "elements" | "elem" => "elem".to_string(),
        "node" | "nodes" => "node".to_string(),
        "child" | "children" => "child".to_string(),
        "parent" => "parent".to_string(),
        "next" | "prev" | "previous" => prop.to_string(),
        "first" | "last" => prop.to_string(),
        "start" | "end" | "begin" => prop.to_string(),
        "count" | "total" | "size" => "count".to_string(),
        "width" | "height" => prop.to_string(),
        "x" | "y" | "z" => prop.to_string(),
        "left" | "right" | "top" | "bottom" => prop.to_string(),
        "config" | "configuration" | "settings" | "options" => "config".to_string(),
        "state" => "state".to_string(),
        "props" | "properties" => "props".to_string(),
        "context" | "ctx" => "ctx".to_string(),
        "callback" | "cb" => "callback".to_string(),
        "handler" => "handler".to_string(),
        "listener" => "listener".to_string(),
        "event" | "evt" | "e" => "event".to_string(),
        "request" | "req" => "req".to_string(),
        "response" | "res" => "res".to_string(),
        "body" => "body".to_string(),
        "headers" => "headers".to_string(),
        "url" | "uri" | "href" => "url".to_string(),
        "path" | "pathname" => "path".to_string(),
        "query" | "search" => "query".to_string(),
        "params" | "parameters" => "params".to_string(),
        "args" | "arguments" => "args".to_string(),
        _ => {
            // Use property name if reasonable
            if prop.len() <= 15 && prop.chars().all(|c| c.is_alphanumeric() || c == '_') {
                sanitize_name(prop)
            } else {
                "prop".to_string()
            }
        }
    }
}

pub fn name_for_instance(class_name: &str) -> String {
    // Convert PascalCase to camelCase
    let mut chars = class_name.chars();
    match chars.next() {
        Some(first) => {
            let lower_first = first.to_lowercase().to_string();
            let rest: String = chars.collect();
            let base = format!("{lower_first}{rest}");
            sanitize_name(&base)
        }
        None => "instance".to_string(),
    }
}

pub fn sanitize_name(name: &str) -> String {
    // Remove invalid characters and ensure valid JS identifier
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    if cleaned.is_empty() {
        return "var".to_string();
    }

    // Ensure doesn't start with a digit
    if cleaned
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        return format!("v{cleaned}");
    }

    // Check for reserved words
    if crate::constants::is_reserved_word(&cleaned) {
        return format!("_{cleaned}");
    }

    // Check for builtin global names (Object, String, Number, Array, etc.)
    // to avoid collisions like String.String(), Object.Object()
    if crate::ir::expr::display::is_builtin_global(&cleaned) {
        return format!("_{cleaned}");
    }

    cleaned
}

#[cfg(test)]
mod factory_prefix_tests {
    use super::*;

    #[test]
    fn strips_factory_and_hook_prefixes() {
        assert_eq!(strip_factory_prefix("createUpdate"), Some("update".to_string()));
        assert_eq!(strip_factory_prefix("useState"), Some("state".to_string()));
        assert_eq!(strip_factory_prefix("useSharedValue"), Some("sharedValue".to_string()));
        assert_eq!(strip_factory_prefix("getUserName"), Some("userName".to_string()));
        // name_for_call wires it in for unknown calls
        assert_eq!(name_for_call("useRef"), "ref");
        assert_eq!(name_for_call("makeStore"), "store");
    }

    #[test]
    fn leaves_non_prefixed_and_unsafe_alone() {
        // no camelCase boundary after the prefix → not a real prefix
        assert_eq!(strip_factory_prefix("getter"), None);
        assert_eq!(strip_factory_prefix("used"), None);
        // would strip to a reserved word
        assert_eq!(strip_factory_prefix("getNew"), None);
        // not a known prefix → generic *Result fallback
        assert_eq!(name_for_call("frobnicate"), "frobnicateResult");
    }
}
