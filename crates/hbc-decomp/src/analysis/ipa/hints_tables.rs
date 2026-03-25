// Map method names to parameter type hints.
// E.g. if a parameter has `.push()` called on it, it's likely an array.
pub(super) fn param_name_from_method(method: &str) -> Option<&'static str> {
    match method {
        "push" | "pop" | "shift" | "unshift" | "splice" | "slice" | "map" | "filter"
        | "reduce" | "find" | "findIndex" | "some" | "every" | "forEach" | "flat" | "flatMap"
        | "sort" | "reverse" | "includes" | "indexOf" | "lastIndexOf" | "fill" | "copyWithin"
        | "entries" | "keys" | "values" | "at" | "concat" => Some("arr"),
        "split" | "trim" | "trimStart" | "trimEnd" | "toLowerCase" | "toUpperCase" | "charAt"
        | "charCodeAt" | "codePointAt" | "substring" | "substr" | "startsWith" | "endsWith"
        | "padStart" | "padEnd" | "repeat" | "normalize" | "match" | "matchAll" | "search"
        | "replace" | "replaceAll" => Some("str"),
        "then" | "catch" | "finally" => Some("promise"),
        "has" | "get" | "set" | "delete" | "clear" | "size" => None,
        "next" | "return" => None,
        _ => None,
    }
}

// Check if a property name is too generic to be useful as a parameter name hint.
pub(super) fn is_generic_property(prop: &str) -> bool {
    matches!(
        prop,
        "length" | "prototype" | "constructor" | "toString" | "valueOf"
            | "hasOwnProperty" | "isPrototypeOf" | "propertyIsEnumerable"
            | "__proto__" | "default"
    )
}

// Map typeof result strings to parameter name hints.
pub(super) fn type_string_to_param_name(type_str: &str) -> Option<&'static str> {
    match type_str {
        "string" => Some("str"),
        "number" => Some("num"),
        "boolean" => Some("flag"),
        "function" => Some("fn"),
        "object" => Some("obj"),
        _ => None,
    }
}
