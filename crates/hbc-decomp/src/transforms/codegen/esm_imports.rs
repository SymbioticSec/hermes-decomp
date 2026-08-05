// ESM import header consolidation.
//
// A module factory emits one import line per require site, so a dependency required
// from many functions produces the same `import X from "M";` line many times, and a
// module accessed by several named members produces several `import { a } from "M";`
// lines. This module folds that header down: exact-duplicate lines collapse to one,
// and distinct named imports of the same module merge into a single
// `import { a, b } from "M";`.

use std::collections::{HashMap, HashSet};

// Collapse the import header a module accumulates. Two forces produce redundant
// lines: the same dependency is required from many functions (identical lines), and
// module-name inference gives several distinct dependency ids the same name, so a
// binding like `_curry2` is imported once per id. Because the module body already
// collapsed those to a single `_curry2` local, emitting one `import _curry2 from
// "_curry2"` per id is not only noise, it is invalid JS (a binding declared many
// times). So imports are de-duplicated by their binding and specifier NAME, ignoring
// the trailing `/* id */` annotation, keeping the first line (hence the first id) as
// the remap hint. Distinct named imports of the same specifier merge into one
// `import { a, b } from "M"`. Order is preserved by first appearance.
pub(super) fn consolidate_imports(imports: Vec<String>) -> Vec<String> {
    enum Slot {
        Line(String),
        Group(usize),
    }
    struct Group {
        specs: Vec<String>,
        tail: String,
    }
    let mut order: Vec<Slot> = Vec::new();
    let mut groups: Vec<Group> = Vec::new();
    // Named-import groups keyed by specifier NAME (id-stripped), so same-module
    // named imports merge and keep the first id.
    let mut group_index: HashMap<String, usize> = HashMap::new();
    // Non-named imports (default / namespace / side-effect / passthrough) keyed by
    // the id-stripped line, so repeats of the same binding collapse to the first.
    let mut seen_lines: HashSet<String> = HashSet::new();

    for line in imports {
        match parse_named_import(&line) {
            Some((specs, tail)) => {
                let key = strip_id_comment(&tail);
                let gi = *group_index.entry(key).or_insert_with(|| {
                    groups.push(Group { specs: Vec::new(), tail: tail.clone() });
                    order.push(Slot::Group(groups.len() - 1));
                    groups.len() - 1
                });
                for s in specs {
                    if !groups[gi].specs.contains(&s) {
                        groups[gi].specs.push(s);
                    }
                }
            }
            None => {
                if seen_lines.insert(strip_id_comment(&line)) {
                    order.push(Slot::Line(line));
                }
            }
        }
    }

    order
        .into_iter()
        .map(|slot| match slot {
            Slot::Line(l) => l,
            Slot::Group(gi) => {
                let g = &groups[gi];
                format!("import {{ {} }} {}", g.specs.join(", "), g.tail)
            }
        })
        .collect()
}

// Remove a trailing ` /* 123 */` module-id annotation so two imports of the same
// binding but different inferred ids compare equal. Only strips the digits-only
// comment immediately before the closing `;`.
fn strip_id_comment(line: &str) -> String {
    if let Some(rest) = line.strip_suffix(";") {
        if let Some(pos) = rest.rfind(" /* ") {
            let inside = &rest[pos + 4..];
            if let Some(num) = inside.strip_suffix(" */") {
                if !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()) {
                    return format!("{};", &rest[..pos]);
                }
            }
        }
    }
    line.to_string()
}

// Parse a plain named import `import { a, b as c } from "src" /* id */;` into its
// specifier list and the `} from "src" ...;`-style tail used as the merge key.
// Returns None for default, namespace, side-effect, or malformed imports.
fn parse_named_import(line: &str) -> Option<(Vec<String>, String)> {
    let rest = line.strip_prefix("import { ")?;
    let boundary = rest.find(" } from ")?;
    let inner = &rest[..boundary];
    // Skip the ` } ` separator; tail is `from "src" ...;`, re-joined as
    // `import { specs } from ...`.
    let tail = &rest[boundary + 3..];
    let specs: Vec<String> = inner
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if specs.is_empty() {
        return None;
    }
    Some((specs, tail.to_string()))
}

#[cfg(test)]
mod tests {
    use super::consolidate_imports;

    #[test]
    fn collapses_exact_duplicate_default_imports() {
        let input = vec![
            "import _curry2 from \"_curry2\" /* 3251 */;".to_string(),
            "import _curry2 from \"_curry2\" /* 3251 */;".to_string(),
            "import _curry2 from \"_curry2\" /* 3251 */;".to_string(),
        ];
        assert_eq!(
            consolidate_imports(input),
            vec!["import _curry2 from \"_curry2\" /* 3251 */;".to_string()]
        );
    }

    #[test]
    fn merges_named_imports_of_same_module() {
        let input = vec![
            "import { sendRequest } from \"HTTPUtils\" /* 530 */;".to_string(),
            "import { healthcheck } from \"HTTPUtils\" /* 530 */;".to_string(),
        ];
        assert_eq!(
            consolidate_imports(input),
            vec!["import { sendRequest, healthcheck } from \"HTTPUtils\" /* 530 */;".to_string()]
        );
    }

    #[test]
    fn keeps_distinct_sources_separate_and_ordered() {
        let input = vec![
            "import a from \"A\" /* 1 */;".to_string(),
            "import { x } from \"B\" /* 2 */;".to_string(),
            "import a from \"A\" /* 1 */;".to_string(),
            "import { y } from \"B\" /* 2 */;".to_string(),
        ];
        assert_eq!(
            consolidate_imports(input),
            vec![
                "import a from \"A\" /* 1 */;".to_string(),
                "import { x, y } from \"B\" /* 2 */;".to_string(),
            ]
        );
    }

    #[test]
    fn collapses_same_binding_different_ids_keeping_first() {
        // Module-name inference gave 3 distinct dependency ids the same name; the
        // body already uses one `_curry2` local, so the header must be one valid
        // import, keeping the first id as the remap hint.
        let input = vec![
            "import _curry2 from \"_curry2\" /* 3100 */;".to_string(),
            "import _curry2 from \"_curry2\" /* 3159 */;".to_string(),
            "import _curry2 from \"_curry2\" /* 3192 */;".to_string(),
        ];
        assert_eq!(
            consolidate_imports(input),
            vec!["import _curry2 from \"_curry2\" /* 3100 */;".to_string()]
        );
    }

    #[test]
    fn merges_same_specifier_different_ids_keeping_first() {
        let input = vec![
            "import { x } from \"M\" /* 1 */;".to_string(),
            "import { y } from \"M\" /* 2 */;".to_string(),
        ];
        assert_eq!(
            consolidate_imports(input),
            vec!["import { x, y } from \"M\" /* 1 */;".to_string()]
        );
    }

    #[test]
    fn keeps_distinct_bindings_of_same_module() {
        // Different local bindings from the same module are distinct imports.
        let input = vec![
            "import a from \"M\" /* 1 */;".to_string(),
            "import b from \"M\" /* 1 */;".to_string(),
        ];
        assert_eq!(
            consolidate_imports(input),
            vec![
                "import a from \"M\" /* 1 */;".to_string(),
                "import b from \"M\" /* 1 */;".to_string(),
            ]
        );
    }
}
