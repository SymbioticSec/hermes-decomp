// ESM import header consolidation.
//
// A module factory emits one import line per require site, so a dependency required
// from many functions produces the same `import X from "M";` line many times, and a
// module accessed by several named members produces several `import { a } from "M";`
// lines. This module folds that header down: exact-duplicate lines collapse to one,
// and distinct named imports of the same module merge into a single
// `import { a, b } from "M";`.

use std::collections::{HashMap, HashSet};

// Collapse the import header a module accumulates. The same dependency required
// from many functions produces identical lines (same specifier and same `/* id */`);
// those collapse to one. Distinct named imports of the SAME module id merge into
// one `import { a, b } from "M" /* id */`. Two different Metro ids never share a
// group, even if a heuristic once gave them the same name.
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
    // Named-import groups keyed by the full tail (specifier + id), so two Metro
    // ids never merge even if they currently share a name.
    let mut group_index: HashMap<String, usize> = HashMap::new();
    // Non-named imports keyed by the full line so exact duplicates collapse
    // and distinct ids stay distinct.
    let mut seen_lines: HashSet<String> = HashSet::new();

    for line in imports {
        match parse_named_import(&line) {
            Some((specs, tail)) => {
                let key = tail.clone();
                let gi = *group_index.entry(key).or_insert_with(|| {
                    groups.push(Group {
                        specs: Vec::new(),
                        tail: tail.clone(),
                    });
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
                if seen_lines.insert(line.clone()) {
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
    let specs = split_import_specs(inner);
    if specs.is_empty() {
        return None;
    }
    Some((specs, tail.to_string()))
}

// Split named-import specifiers on commas that are outside quotes so
// `import { "foo, bar" as x } from "M"` stays one specifier.
fn split_import_specs(inner: &str) -> Vec<String> {
    let mut specs = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut escape = false;
    for c in inner.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == q {
                quote = None;
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
            cur.push(c);
        } else if c == ',' {
            let t = cur.trim();
            if !t.is_empty() {
                specs.push(t.to_string());
            }
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        specs.push(t.to_string());
    }
    specs
}

// After consolidation: drop `Export as Local` when `Export` is already bound in
// the same import, rewrite the unused alias in the body, and collapse two
// default imports of the same module id to one import plus `const alias = kept`.
pub(super) fn fold_redundant_imports(
    imports: Vec<String>,
    body: &mut [String],
    exports: &mut [String],
) -> (Vec<String>, Vec<String>) {
    let imports = fold_named_aliases(imports, body, exports);
    fold_duplicate_defaults(imports, body, exports)
}

fn fold_named_aliases(
    imports: Vec<String>,
    body: &mut [String],
    exports: &mut [String],
) -> Vec<String> {
    let mut out = Vec::with_capacity(imports.len());
    for line in imports {
        match parse_named_import(&line) {
            Some((specs, tail)) => {
                let (kept, rewrites) = collapse_same_export_specs(&specs);
                apply_rewrites(&rewrites, body, exports);
                if kept.is_empty() {
                    continue;
                }
                out.push(format!("import {{ {} }} {}", kept.join(", "), tail));
            }
            None => out.push(line),
        }
    }
    out
}

// One spec per export name. Prefer the unaliased `Export` when it is a legal
// binding; otherwise keep the first alias. Other locals become rewrites.
fn collapse_same_export_specs(specs: &[String]) -> (Vec<String>, Vec<(String, String)>) {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<String, Vec<(String, String, bool)>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for spec in specs {
        let (export, local, aliased) = split_spec(spec);
        if !groups.contains_key(&export) {
            order.push(export.clone());
        }
        groups
            .entry(export)
            .or_default()
            .push((spec.clone(), local, aliased));
    }
    let mut kept = Vec::new();
    let mut rewrites = Vec::new();
    for export in order {
        let members = &groups[&export];
        let bare = members.iter().find(|(_, _, aliased)| !*aliased);
        let (canonical_spec, canonical_local) = if let Some((spec, local, _)) = bare {
            (spec.clone(), local.clone())
        } else {
            let (spec, local, _) = &members[0];
            (spec.clone(), local.clone())
        };
        kept.push(canonical_spec);
        for (_, local, aliased) in members {
            if *aliased && local != &canonical_local {
                rewrites.push((local.clone(), canonical_local.clone()));
            }
        }
    }
    rewrites.sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()).then(a.cmp(b)));
    (kept, rewrites)
}

fn split_spec(spec: &str) -> (String, String, bool) {
    if let Some((export, local)) = split_as_clause(spec) {
        (export.to_string(), local.to_string(), true)
    } else {
        (spec.to_string(), spec.to_string(), false)
    }
}

fn split_as_clause(spec: &str) -> Option<(&str, &str)> {
    let mut quote: Option<char> = None;
    let mut escape = false;
    for (idx, c) in spec.char_indices() {
        if let Some(q) = quote {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == q {
                quote = None;
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
        } else if spec[idx..].starts_with(" as ") {
            let export = spec[..idx].trim();
            let local = spec[idx + 4..].trim();
            if !export.is_empty() && !local.is_empty() {
                return Some((export, local));
            }
        }
    }
    None
}

fn fold_duplicate_defaults(
    imports: Vec<String>,
    body: &mut [String],
    exports: &mut [String],
) -> (Vec<String>, Vec<String>) {
    use std::collections::HashMap;
    let mut first_local: HashMap<(String, Option<u32>), String> = HashMap::new();
    let mut extras: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for line in imports {
        if let Some((local, spec, id)) = parse_default_import(&line) {
            let key = (spec, id);
            if let Some(kept) = first_local.get(&key) {
                if &local != kept
                    && (contains_whole_word_in(body, &local)
                        || contains_whole_word_in(exports, &local))
                {
                    extras.push(format!("const {local} = {kept};"));
                }
                continue;
            }
            first_local.insert(key, local);
            out.push(line);
        } else {
            out.push(line);
        }
    }
    (out, extras)
}

fn contains_whole_word_in(parts: &[String], word: &str) -> bool {
    parts.iter().any(|p| {
        let replaced = super::replace_whole_word(p, word, "\0");
        replaced != *p
    })
}

fn parse_default_import(line: &str) -> Option<(String, String, Option<u32>)> {
    let rest = line.strip_prefix("import ")?;
    let trimmed = rest.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('*') || trimmed.starts_with('"') {
        return None;
    }
    let (local, after) = rest.split_once(" from \"")?;
    let local = local.trim();
    if local.is_empty() || local.contains(char::is_whitespace) {
        return None;
    }
    let (spec, rest2) = after.split_once('"')?;
    let id = trailing_module_id(rest2);
    Some((local.to_string(), spec.to_string(), id))
}

fn trailing_module_id(s: &str) -> Option<u32> {
    let s = s.trim().trim_end_matches(';').trim();
    let start = s.rfind("/* ")?;
    let inner = s[start + 3..].strip_suffix("*/")?.trim();
    if inner.is_empty() || !inner.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    inner.parse().ok()
}

fn apply_rewrites(rewrites: &[(String, String)], body: &mut [String], exports: &mut [String]) {
    if rewrites.is_empty() {
        return;
    }
    for part in body.iter_mut().chain(exports.iter_mut()) {
        for (old, new_name) in rewrites {
            *part = super::replace_whole_word(part, old, new_name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{consolidate_imports, fold_redundant_imports};

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
    fn keeps_same_binding_different_ids_separate() {
        // Distinct Metro ids must stay distinct even when a heuristic reused the
        // specifier. A2 uniquifies the names so this case should not reach codegen.
        let input = vec![
            "import _curry2 from \"_curry2\" /* 3100 */;".to_string(),
            "import _curry2 from \"_curry2\" /* 3159 */;".to_string(),
            "import _curry2 from \"_curry2\" /* 3192 */;".to_string(),
        ];
        assert_eq!(
            consolidate_imports(input),
            vec![
                "import _curry2 from \"_curry2\" /* 3100 */;".to_string(),
                "import _curry2 from \"_curry2\" /* 3159 */;".to_string(),
                "import _curry2 from \"_curry2\" /* 3192 */;".to_string(),
            ]
        );
    }

    #[test]
    fn keeps_same_specifier_different_ids_separate() {
        let input = vec![
            "import { x } from \"M\" /* 1 */;".to_string(),
            "import { y } from \"M\" /* 2 */;".to_string(),
        ];
        assert_eq!(
            consolidate_imports(input),
            vec![
                "import { x } from \"M\" /* 1 */;".to_string(),
                "import { y } from \"M\" /* 2 */;".to_string(),
            ]
        );
    }

    #[test]
    fn folds_same_export_aliases_and_rewrites_body() {
        let imports = vec![
            "import { MessageType, MessageType as MessageType2, MessageType as MessageType54 } from \"module_1307\" /* 1307 */;".to_string(),
        ];
        let mut body = vec!["use(MessageType, MessageType2, MessageType54);".to_string()];
        let mut exports = Vec::new();
        let (out, extras) = fold_redundant_imports(imports, &mut body, &mut exports);
        assert_eq!(
            out,
            vec!["import { MessageType } from \"module_1307\" /* 1307 */;".to_string()]
        );
        assert!(extras.is_empty());
        assert_eq!(
            body,
            vec!["use(MessageType, MessageType, MessageType);".to_string()]
        );
    }

    #[test]
    fn folds_duplicate_default_imports_of_same_id() {
        let imports = vec![
            "import extractTimestamp from \"extractTimestamp\" /* 13 */;".to_string(),
            "import extractTimestampAll from \"extractTimestamp\" /* 13 */;".to_string(),
        ];
        let mut body = vec!["return extractTimestampAll.age(x);".to_string()];
        let mut exports = Vec::new();
        let (out, extras) = fold_redundant_imports(imports, &mut body, &mut exports);
        assert_eq!(
            out,
            vec!["import extractTimestamp from \"extractTimestamp\" /* 13 */;".to_string()]
        );
        assert_eq!(
            extras,
            vec!["const extractTimestampAll = extractTimestamp;".to_string()]
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

    #[test]
    fn keeps_comma_inside_string_export_name() {
        let input = vec![
            "import { \"foo, bar\" as x } from \"M\" /* 1 */;".to_string(),
            "import { y } from \"M\" /* 1 */;".to_string(),
        ];
        assert_eq!(
            consolidate_imports(input),
            vec!["import { \"foo, bar\" as x, y } from \"M\" /* 1 */;".to_string()]
        );
    }
}

// Give each dependency its own default-import name.
//
// The name comes from what the module inferred locally, and two dependencies in
// one file can infer the same one, so both asked to be imported as
// `defineLazyObjectProperty_mod`. Importing one name twice binds it twice, which
// a parser rejects and the module is lost whole. The first line keeps the name
// and later ones take a numbered form, applied to the body as well since that is
// where the binding is read.
pub(super) fn make_default_imports_distinct(
    imports: &mut [String],
    body: &mut [String],
    exports: &mut [String],
) {
    use std::collections::{HashMap, HashSet};
    use std::sync::OnceLock;
    static DEFAULT_IMPORT: OnceLock<regex::Regex> = OnceLock::new();
    let rx = DEFAULT_IMPORT.get_or_init(|| {
        regex::Regex::new(r#"^import\s+([A-Za-z_$][\w$]*)\s+from\s"#).expect("static")
    });

    // name -> the import line that claimed it first
    let mut owner: HashMap<String, String> = HashMap::new();
    let mut taken: HashSet<String> = HashSet::new();
    let mut renames: Vec<(String, String)> = Vec::new();

    for line in imports.iter() {
        if let Some(c) = rx.captures(line) {
            taken.insert(c[1].to_string());
        }
    }

    for line in imports.iter_mut() {
        let Some(name) = rx.captures(line).map(|c| c[1].to_string()) else {
            continue;
        };
        match owner.get(&name) {
            // The same dependency imported twice collapses elsewhere.
            Some(existing) if existing == line => continue,
            None => {
                owner.insert(name, line.clone());
            }
            Some(_) => {
                let mut n = 2u32;
                let mut candidate = format!("{name}{n}");
                while !taken.insert(candidate.clone()) {
                    n += 1;
                    candidate = format!("{name}{n}");
                }
                *line = replace_word(line, &name, &candidate);
                owner.insert(candidate.clone(), line.clone());
                renames.push((name, candidate));
            }
        }
    }

    if renames.is_empty() {
        return;
    }
    // The renamed binding is read in the body, so it has to follow.
    for (from, to) in &renames {
        for chunk in body.iter_mut().chain(exports.iter_mut()) {
            *chunk = replace_word(chunk, from, to);
        }
    }
}

// Replace whole-word occurrences only, so `X` never rewrites part of `X_other`.
fn replace_word(haystack: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(haystack.len());
    let bytes = haystack.as_bytes();
    let mut i = 0;
    while let Some(pos) = haystack[i..].find(from) {
        let start = i + pos;
        let end = start + from.len();
        let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_ident_byte(bytes[end]);
        out.push_str(&haystack[i..start]);
        if before_ok && after_ok {
            out.push_str(to);
        } else {
            out.push_str(from);
        }
        i = end;
    }
    out.push_str(&haystack[i..]);
    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

#[cfg(test)]
mod distinct_default_import_tests {
    use super::make_default_imports_distinct;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn two_dependencies_asking_for_one_name_are_separated() {
        // Both lines bind the same name, which a parser rejects outright.
        let mut imports = v(&[
            "import prop_mod from \"defineLazyObjectProperty\";",
            "import prop_mod from \"module_91\";",
        ]);
        let mut body = v(&["let prop = prop_mod;\n"]);
        let mut exports = v(&[]);
        make_default_imports_distinct(&mut imports, &mut body, &mut exports);
        assert_eq!(
            imports[0],
            "import prop_mod from \"defineLazyObjectProperty\";"
        );
        assert_eq!(imports[1], "import prop_mod2 from \"module_91\";");
    }

    #[test]
    fn the_body_follows_the_rename() {
        let mut imports = v(&["import m from \"a\";", "import m from \"b\";"]);
        let mut body = v(&["let x = m;\n", "use(m, m_other);\n"]);
        let mut exports = v(&["export const y = m;"]);
        make_default_imports_distinct(&mut imports, &mut body, &mut exports);
        // Only whole words move, so `m_other` is left alone.
        assert_eq!(body[1], "use(m2, m_other);\n");
        assert_eq!(exports[0], "export const y = m2;");
    }

    #[test]
    fn the_same_dependency_twice_is_left_to_the_usual_collapse() {
        let mut imports = v(&["import m from \"a\";", "import m from \"a\";"]);
        let mut body = v(&["let x = m;\n"]);
        let mut exports = v(&[]);
        make_default_imports_distinct(&mut imports, &mut body, &mut exports);
        assert_eq!(
            imports[1], "import m from \"a\";",
            "no rename for one dependency"
        );
        assert_eq!(body[0], "let x = m;\n");
    }

    #[test]
    fn a_named_import_is_not_mistaken_for_a_default_one() {
        // `import { a as b } from` starts with a brace, and reading that brace as
        // the name rewrote lines into `import {2 ...`.
        let mut imports = v(&[
            "import { a as b } from \"x\";",
            "import { c as d } from \"y\";",
        ]);
        let before = imports.clone();
        let mut body = v(&[]);
        let mut exports = v(&[]);
        make_default_imports_distinct(&mut imports, &mut body, &mut exports);
        assert_eq!(imports, before);
    }
}
