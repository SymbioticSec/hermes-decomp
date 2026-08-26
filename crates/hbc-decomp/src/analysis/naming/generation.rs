use super::registers::{RegisterInfo, RegisterRole};
use std::collections::HashSet;

pub(crate) fn infer_type_from_properties(props: &HashSet<String>) -> Option<&'static str> {
    // Property signatures ranked by specificity (most specific first).
    // Each entry: (candidate_props, min_matches, inferred_name)
    const SIGNATURES: &[(&[&str], usize, &str)] = &[
        (&["latitude", "longitude"], 2, "location"),
        (&["email", "username", "password"], 2, "user"),
        (&["status", "headers", "statusCode"], 2, "response"),
        (&["message", "stack"], 2, "error"),
        (&["width", "height"], 2, "size"),
        (&["x", "y", "z"], 2, "point"),
        (&["key", "value"], 2, "entry"),
        (&["left", "right", "top", "bottom"], 2, "rect"),
        (&["host", "port", "protocol", "pathname", "hostname"], 2, "url"),
        (&["method", "url", "body"], 2, "request"),
        (&["url", "query"], 2, "request"),
        (&["params", "query", "route"], 2, "route"),
        (&["children", "props", "type"], 2, "element"),
        (&["dispatch", "getState", "subscribe"], 2, "store"),
        (&["navigate", "goBack", "reset"], 2, "navigation"),
        // Additional clusters (appended so earlier, previously-shipped signatures
        // keep priority). Each is specific enough to be unambiguous.
        (&["red", "green", "blue", "alpha"], 3, "color"),
        (&["hours", "minutes", "seconds"], 2, "time"),
        (&["day", "month", "year"], 2, "date"),
        (&["type", "payload"], 2, "action"),
        (&["min", "max"], 2, "range"),
    ];

    for (candidates, min, name) in SIGNATURES {
        let matches = candidates.iter().filter(|p| props.contains(**p)).count();
        if matches >= *min {
            return Some(name);
        }
    }
    None
}

// A distinctive method name whose presence strongly implies the object's role,
// mapped to an agent-noun base name. Deliberately conservative: only verbs where
// the "-er"/"-or" noun is essentially definitional. Generic methods (toString,
// valueOf, hasOwnProperty, call, apply, bind, then, catch, …) are intentionally
// absent so registers used only through them stay generic (`obj`/`tmp`).
fn infer_name_from_methods(methods: &HashSet<String>) -> Option<&'static str> {
    const METHOD_NAMES: &[(&str, &str)] = &[
        // Codec-definitional only: an object that exposes `.encode()` essentially
        // is an encoder. Verbs like render/validate/compile are intentionally left
        // out: React components expose `.render()`, schemas expose `.validate()`,
        // templates expose `.compile()`, so those would mislabel the object.
        ("encode", "encoder"),
        ("decode", "decoder"),
        ("serialize", "serializer"),
        ("deserialize", "deserializer"),
        ("tokenize", "tokenizer"),
        ("normalize", "normalizer"),
        ("sanitize", "sanitizer"),
    ];
    for (method, name) in METHOD_NAMES {
        if methods.contains(*method) {
            return Some(name);
        }
    }
    None
}

// Turn a source property name into a safe identifier base, or `None` if it is not
// usable. Digits become `vN`; builtin globals / reserved words are prefixed with
// `_`; anything that is not a valid identifier is rejected.
fn semantic_ident(prop: &str) -> Option<String> {
    if prop.is_empty() {
        return None;
    }
    if prop.chars().all(|c| c.is_ascii_digit()) {
        return Some(format!("v{prop}"));
    }
    if !crate::util::is_valid_identifier(prop) {
        return None;
    }
    if crate::ir::expr::display::is_builtin_global(prop)
        || crate::constants::is_reserved_word(prop)
    {
        return Some(format!("_{prop}"));
    }
    Some(prop.to_string())
}

// A register must be read at least twice before a distinctive-method name is
// assigned. Single-use registers are prime inlining candidates; renaming them to
// a non-generic (non-inlinable) name would suppress that inlining and leave the
// value — plus any sibling temporaries — behind, growing the output rather than
// shrinking it. Multi-use registers are genuine survivors, so naming them is a
// direct win.
const MIN_USES_FOR_METHOD_NAME: usize = 2;

pub fn generate_name(info: &RegisterInfo, used_names: &mut HashSet<String>) -> String {
    // Priority: destructuring key name (e.g., { email: r10001 } → "email")
    if let Some(key) = &info.destructuring_key {
        if !key.is_empty() && crate::util::is_valid_identifier(key) {
            let base = if crate::ir::expr::display::is_builtin_global(key)
                || crate::constants::is_reserved_word(key) {
                format!("_{key}")
            } else {
                key.clone()
            };
            return make_unique(base, used_names);
        }
    }

    let base = match &info.role {
        RegisterRole::Array => "arr",
        RegisterRole::Object => {
            if let Some(type_name) = infer_type_from_properties(&info.accessed_props) {
                return make_unique(type_name.to_string(), used_names);
            }
            if info.use_count >= MIN_USES_FOR_METHOD_NAME {
                if let Some(type_name) = infer_name_from_methods(&info.called_methods) {
                    return make_unique(type_name.to_string(), used_names);
                }
            }
            "obj"
        }
        RegisterRole::Function => {
            // A named function value (Babel/Metro helper like `_typeof`) keeps its
            // own name instead of the generic "fn".
            if let Some(fname) = &info.function_name {
                return make_unique(fname.clone(), used_names);
            }
            "fn"
        }
        RegisterRole::String => "str",
        RegisterRole::Number => "num",
        RegisterRole::Boolean => "flag",
        RegisterRole::BigInt => "bigint",
        RegisterRole::Iterator => "iter",
        RegisterRole::Promise => "promise",
        RegisterRole::This => "self",
        RegisterRole::Null | RegisterRole::Undefined => "tmp",
        RegisterRole::Unknown => {
            if let Some(prop) = &info.from_property {
                if let Some(base) = semantic_ident(prop) {
                    return make_unique(base, used_names);
                }
            }
            let method_name = if info.use_count >= MIN_USES_FOR_METHOD_NAME {
                infer_name_from_methods(&info.called_methods)
            } else {
                None
            };
            if info.accessed_props.contains("length") && info.called_methods.contains("push") {
                "arr"
            } else if let Some(type_name) = infer_type_from_properties(&info.accessed_props) {
                return make_unique(type_name.to_string(), used_names);
            } else if let Some(type_name) = method_name {
                return make_unique(type_name.to_string(), used_names);
            } else if !info.called_methods.is_empty() {
                "obj"
            } else {
                "tmp"
            }
        }
    };

    make_unique(base.to_string(), used_names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_user_type() {
        let props: HashSet<String> = ["email", "password"].iter().map(|s| s.to_string()).collect();
        assert_eq!(infer_type_from_properties(&props), Some("user"));
    }

    #[test]
    fn test_infer_error_type() {
        let props: HashSet<String> = ["message", "stack"].iter().map(|s| s.to_string()).collect();
        assert_eq!(infer_type_from_properties(&props), Some("error"));
    }

    #[test]
    fn test_infer_response_type() {
        let props: HashSet<String> = ["status", "headers"].iter().map(|s| s.to_string()).collect();
        assert_eq!(infer_type_from_properties(&props), Some("response"));
    }

    #[test]
    fn test_infer_point_type() {
        let props: HashSet<String> = ["x", "y"].iter().map(|s| s.to_string()).collect();
        assert_eq!(infer_type_from_properties(&props), Some("point"));
    }

    #[test]
    fn test_infer_no_match() {
        let props: HashSet<String> = ["foo", "bar"].iter().map(|s| s.to_string()).collect();
        assert_eq!(infer_type_from_properties(&props), None);
    }

    #[test]
    fn test_infer_http_query_request() {
        let props: HashSet<String> = ["url", "query", "oldFormErrors"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(infer_type_from_properties(&props), Some("request"));
    }

    #[test]
    fn test_generate_object_with_props() {
        let mut used = HashSet::new();
        let info = RegisterInfo {
            role: RegisterRole::Object,
            accessed_props: ["email", "password"].iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let name = generate_name(&info, &mut used);
        assert_eq!(name, "user");
    }

    #[test]
    fn test_generate_object_without_props() {
        let mut used = HashSet::new();
        let info = RegisterInfo {
            role: RegisterRole::Object,
            accessed_props: HashSet::new(),
            ..Default::default()
        };
        let name = generate_name(&info, &mut used);
        assert_eq!(name, "obj");
    }

    #[test]
    fn test_named_function_keeps_its_name() {
        let mut used = HashSet::new();
        let info = RegisterInfo {
            role: RegisterRole::Function,
            function_name: Some("_typeof".to_string()),
            ..Default::default()
        };
        assert_eq!(generate_name(&info, &mut used), "_typeof");
    }

    #[test]
    fn test_anonymous_function_falls_back_to_fn() {
        let mut used = HashSet::new();
        let info = RegisterInfo {
            role: RegisterRole::Function,
            ..Default::default()
        };
        assert_eq!(generate_name(&info, &mut used), "fn");
    }

    #[test]
    fn test_infer_action_type() {
        let props: HashSet<String> = ["type", "payload"].iter().map(|s| s.to_string()).collect();
        assert_eq!(infer_type_from_properties(&props), Some("action"));
    }

    #[test]
    fn test_infer_range_type() {
        let props: HashSet<String> = ["min", "max"].iter().map(|s| s.to_string()).collect();
        assert_eq!(infer_type_from_properties(&props), Some("range"));
    }

    #[test]
    fn test_method_encoder() {
        let methods: HashSet<String> = ["encode"].iter().map(|s| s.to_string()).collect();
        assert_eq!(infer_name_from_methods(&methods), Some("encoder"));
    }

    #[test]
    fn test_generic_method_stays_none() {
        let methods: HashSet<String> =
            ["toString", "valueOf"].iter().map(|s| s.to_string()).collect();
        assert_eq!(infer_name_from_methods(&methods), None);
    }

    #[test]
    fn test_unknown_multiuse_distinctive_method_named() {
        let mut used = HashSet::new();
        let info = RegisterInfo {
            role: RegisterRole::Unknown,
            called_methods: ["decode"].iter().map(|s| s.to_string()).collect(),
            use_count: 2,
            ..Default::default()
        };
        assert_eq!(generate_name(&info, &mut used), "decoder");
    }

    #[test]
    fn test_unknown_singleuse_method_stays_obj() {
        // Single-use registers stay generic so downstream inlining still fires.
        let mut used = HashSet::new();
        let info = RegisterInfo {
            role: RegisterRole::Unknown,
            called_methods: ["decode"].iter().map(|s| s.to_string()).collect(),
            use_count: 1,
            ..Default::default()
        };
        assert_eq!(generate_name(&info, &mut used), "obj");
    }

    #[test]
    fn test_unknown_with_generic_method_stays_obj() {
        let mut used = HashSet::new();
        let info = RegisterInfo {
            role: RegisterRole::Unknown,
            called_methods: ["toString"].iter().map(|s| s.to_string()).collect(),
            use_count: 3,
            ..Default::default()
        };
        assert_eq!(generate_name(&info, &mut used), "obj");
    }

    #[test]
    fn test_unknown_from_property_named() {
        let mut used = HashSet::new();
        let info = RegisterInfo {
            role: RegisterRole::Unknown,
            from_property: Some("fooBar".to_string()),
            ..Default::default()
        };
        assert_eq!(generate_name(&info, &mut used), "fooBar");
    }

    #[test]
    fn test_no_signal_stays_tmp() {
        let mut used = HashSet::new();
        let info = RegisterInfo {
            role: RegisterRole::Unknown,
            ..Default::default()
        };
        assert_eq!(generate_name(&info, &mut used), "tmp");
    }

    #[test]
    fn test_null_stays_tmp() {
        let mut used = HashSet::new();
        let info = RegisterInfo {
            role: RegisterRole::Undefined,
            ..Default::default()
        };
        assert_eq!(generate_name(&info, &mut used), "tmp");
    }

    #[test]
    fn test_reserved_property_gets_underscore() {
        let mut used = HashSet::new();
        let info = RegisterInfo {
            role: RegisterRole::Unknown,
            from_property: Some("default".to_string()),
            ..Default::default()
        };
        assert_eq!(generate_name(&info, &mut used), "_default");
    }
}

fn make_unique(base: String, used: &mut HashSet<String>) -> String {
    if !used.contains(&base) {
        used.insert(base.clone());
        return base;
    }

    // Unbounded: a function with hundreds of same-role registers (e.g. a Lottie
    // animation data module with deeply nested array/object literals) needs more
    // than a fixed handful of suffixes. A previous `2..100` cap fell back to the
    // bare `base` once exhausted, so distinct live arrays collapsed to one name and
    // produced self-referential garbage like `items[3] = items`.
    let mut i = 2u32;
    loop {
        let name = format!("{base}{i}");
        if !used.contains(&name) {
            used.insert(name.clone());
            return name;
        }
        i += 1;
    }
}
