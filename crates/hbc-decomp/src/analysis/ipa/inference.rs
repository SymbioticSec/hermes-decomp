use crate::ir::{Expression, PropertyKey, Value};
use std::collections::HashMap;
use std::collections::BTreeMap;

// Votes on the best parameter name for each argument index.
//
// Context:
// A function might be called from multiple places (call sites).
// Each call site might provide different hints for naming parameters:
// - Site A: `login(email, pass)` -> hints: ["email", "pass"]
// - Site B: `login(user.email, p)` -> hints: ["email", "p"]
//
// We collect all these hints and "vote" to find the most common name for each position.
// We ignore generic names (like "p", "arg0") if better names are available.
// Consistently used names (e.g., "email" appearing in 90% of calls) will win.
pub fn vote_on_names(sites: Vec<Vec<Option<String>>>) -> Vec<Option<String>> {
    let max_args = sites.iter().map(|s| s.len()).max().unwrap_or(0);
    let mut param_names_map: HashMap<usize, HashMap<String, usize>> = HashMap::new();

    for arg_idx in 0..max_args {
        for site in &sites {
            if let Some(Some(name)) = site.get(arg_idx) {
                if !is_generic_name(name) {
                    *param_names_map
                        .entry(arg_idx)
                        .or_default()
                        .entry(name.clone())
                        .or_insert(0) += 1;
                }
            }
        }
    }

    let mut final_names = vec![None; max_args];
    let mut used_names = HashMap::new();

    for (arg_idx, final_name) in final_names.iter_mut().enumerate() {
        if let Some(name_counts) = param_names_map.get(&arg_idx) {
            if let Some((name, _)) = name_counts.iter().max_by(|(n1, c1), (n2, c2)| c1.cmp(c2).then_with(|| n2.cmp(n1))) {
                let mut chosen = name.clone();
                if let Some(count) = used_names.get(name) {
                    chosen = format!("{}{}", name, count + 1);
                }
                *final_name = Some(chosen);
                *used_names.entry(name.clone()).or_insert(0) += 1;
            }
        }
    }
    final_names
}

// Recursively walks an expression to find parameter name hints.
//
// Helps handling:
// - Object literals: `{ email: arg0 }` -> arg0 should be named "email"
// - Array elements: `[arg0]` -> weak hint, maybe "element"
// - Binary ops: `arg0 + 10` -> no strong hint
pub fn collect_param_names_from_expr(
    expr: &Expression,
    owner_id: u32,
    self_param_names: &mut BTreeMap<u32, Vec<Vec<Option<String>>>>,
) {
    let mut site_results = Vec::new();
    walk_expr_for_params(expr, None, &mut site_results);

    if !site_results.is_empty() {
        // Flatten the results into a single "site" vector for voting
        let max_idx = (site_results.iter().map(|(idx, _)| *idx).max().unwrap_or(0) as usize)
            .min(super::MAX_PARAM_SLOTS);
        let mut site = vec![None; max_idx + 1];
        for (idx, name) in site_results {
            let idx = idx as usize;
            if idx < site.len() && site[idx].is_none() {
                site[idx] = Some(name);
            }
        }
        self_param_names.entry(owner_id).or_default().push(site);
    }
}

fn walk_expr_for_params(
    expr: &Expression,
    current_suggestion: Option<&str>,
    results: &mut Vec<(u32, String)>,
) {
    match expr {
        Expression::Value(Value::Parameter(idx)) => {
            if let Some(suggestion) = current_suggestion {
                results.push((*idx, suggestion.to_string()));
            }
        }
        Expression::Object { properties } => {
            for prop in properties {
                if let PropertyKey::Ident(name) = &prop.key {
                    walk_expr_for_params(&prop.value, Some(name), results);
                } else {
                    walk_expr_for_params(&prop.value, None, results);
                }
            }
        }
        Expression::Array { elements } => {
            for e in elements.iter().flatten() {
                walk_expr_for_params(e, current_suggestion, results);
            }
        }
        Expression::Binary { left, right, .. } => {
            walk_expr_for_params(left, None, results);
            walk_expr_for_params(right, None, results);
        }
        Expression::Unary { operand, .. } => {
            walk_expr_for_params(operand, current_suggestion, results);
        }
        Expression::Member { object, .. } => {
            walk_expr_for_params(object, current_suggestion, results);
        }
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            walk_expr_for_params(callee, None, results);
            for arg in arguments {
                walk_expr_for_params(arg, None, results);
            }
        }
        Expression::Assignment { value, .. } => {
            walk_expr_for_params(value, current_suggestion, results);
        }
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_expr_for_params(condition, None, results);
            walk_expr_for_params(then_expr, current_suggestion, results);
            walk_expr_for_params(else_expr, current_suggestion, results);
        }
        Expression::Spread(inner) => {
            walk_expr_for_params(inner, current_suggestion, results);
        }
        _ => {}
    }
}

pub fn is_generic_name(name: &str) -> bool {
    // Check for generic prefixes like r0, arg1, var2, etc.
    let prefixes = ["r", "t", "arg", "var", "val", "tmp", "obj", "str", "num"];
    for &prefix in &prefixes {
        if let Some(rest) = name.strip_prefix(prefix) {
            if rest.is_empty() {
                return true;
            } // e.g. "arg"
            if rest.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
    }

    // Names that are too short/generic for parameter inference
    let generic_vars = [
        "_", "__", "$$", "e", "a", "b", "c", "i", "j", "k", "n", "x", "y", "z",
        "tmp", "temp",
    ];
    if generic_vars.contains(&name) {
        return true;
    }

    // Check for JS reserved keywords
    let reserved = [
        "default",
        "this",
        "super",
        "class",
        "extends",
        "const",
        "let",
        "var",
        "function",
        "return",
        "if",
        "else",
        "for",
        "while",
        "do",
        "switch",
        "case",
        "break",
        "continue",
        "throw",
        "try",
        "catch",
        "finally",
        "new",
        "delete",
        "typeof",
        "void",
        "in",
        "instanceof",
        "of",
        "true",
        "false",
        "null",
        "undefined",
        "NaN",
        "Infinity",
        "async",
        "await",
        "yield",
        "import",
        "export",
        "from",
        "as",
        "with",
        "debugger",
        "enum",
        "implements",
        "interface",
        "package",
        "private",
        "protected",
        "public",
        "static",
    ];
    if reserved.contains(&name) {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vote_on_names_basic() {
        let sites = vec![
            vec![Some("email".into()), Some("password".into())],
            vec![Some("email".into()), Some("pass".into())],
        ];
        let result = vote_on_names(sites);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], Some("email".to_string()));
        // "password" and "pass" each appear once, either is acceptable
        assert!(result[1].is_some());
    }

    #[test]
    fn test_vote_on_names_ignores_generic() {
        let sites = vec![
            vec![Some("arg0".into())],
            vec![Some("email".into())],
        ];
        let result = vote_on_names(sites);
        assert_eq!(result[0], Some("email".to_string()));
    }

    #[test]
    fn test_vote_on_names_empty() {
        let sites: Vec<Vec<Option<String>>> = vec![];
        let result = vote_on_names(sites);
        assert!(result.is_empty());
    }

    #[test]
    fn test_is_generic_name() {
        assert!(is_generic_name("arg0"));
        assert!(is_generic_name("tmp"));
        assert!(is_generic_name("obj"));
        assert!(is_generic_name("r123"));
        assert!(is_generic_name("e"));
        assert!(is_generic_name("i"));
        assert!(!is_generic_name("email"));
        assert!(!is_generic_name("response"));
        assert!(!is_generic_name("user"));
    }

    #[test]
    fn test_is_generic_name_reserved() {
        assert!(is_generic_name("default"));
        assert!(is_generic_name("this"));
        assert!(is_generic_name("undefined"));
    }
}
