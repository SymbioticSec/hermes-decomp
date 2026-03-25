use super::registers::{RegisterInfo, RegisterRole};
use std::collections::HashSet;

fn infer_type_from_properties(props: &HashSet<String>) -> Option<&'static str> {
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
        (&["params", "query", "route"], 2, "route"),
        (&["children", "props", "type"], 2, "element"),
        (&["dispatch", "getState", "subscribe"], 2, "store"),
        (&["navigate", "goBack", "reset"], 2, "navigation"),
    ];

    for (candidates, min, name) in SIGNATURES {
        let matches = candidates.iter().filter(|p| props.contains(**p)).count();
        if matches >= *min {
            return Some(name);
        }
    }
    None
}

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
            "obj"
        }
        RegisterRole::Function => "fn",
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
                let base = if prop.chars().all(|c| c.is_ascii_digit()) {
                    format!("v{prop}")
                } else if crate::ir::expr::display::is_builtin_global(prop)
                    || crate::constants::is_reserved_word(prop) {
                    format!("_{prop}")
                } else {
                    prop.clone()
                };
                return make_unique(base, used_names);
            }
            if info.accessed_props.contains("length") && info.called_methods.contains("push") {
                "arr"
            } else if let Some(type_name) = infer_type_from_properties(&info.accessed_props) {
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
}

fn make_unique(base: String, used: &mut HashSet<String>) -> String {
    if !used.contains(&base) {
        used.insert(base.clone());
        return base;
    }

    for i in 2..100 {
        let name = format!("{base}{i}");
        if !used.contains(&name) {
            used.insert(name.clone());
            return name;
        }
    }

    base
}
