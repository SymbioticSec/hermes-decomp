// Semantically-meaningful constants used across multiple analysis and transformation passes.
// Each constant is based on JavaScript language semantics, not arbitrary choices.

// Transformation methods - methods that produce a NEW value from the input.
//
// **Semantic rule:** When you call `input.method()`, the RESULT is different
// from the INPUT. Therefore, the INPUT variable name doesn't describe the OUTPUT.
//
// Example:
// ```javascript
// let arr = [1, 2, 3, 4, 5];
// let filtered = arr.filter(x => x > 2);  // arr is [1,2,3,4,5], filtered is [3,4,5]
// ```
//
// If we see `arg0.filter(...)`, we should NOT rename `arg0` to `filter` because:
// - `filter` is the OPERATION, not what `arg0` contains
// - `arg0` is the INPUT array, NOT the filtered result
//
// Used by:
// - `var_naming/analysis.rs` - skip renaming variables based on method access
// - `ipa/traversal.rs` - skip using object name for param inference
pub const TRANSFORMATION_METHODS: &[&str] = &[
    // Array methods that transform data
    "filter",
    "map",
    "reduce",
    "forEach",
    "find",
    "findIndex",
    "some",
    "every",
    "includes",
    "indexOf",
    "lastIndexOf",
    "join",
    "slice",
    "splice",
    "concat",
    "push",
    "pop",
    "shift",
    "unshift",
    "sort",
    "reverse",
    "flat",
    "flatMap",
    "fill",
    "copyWithin",
    "entries",
    "keys",
    "values",
    "at",
    // String methods
    "split",
    "replace",
    "replaceAll",
    "match",
    "search",
    "trim",
    "trimStart",
    "trimEnd",
    "toLowerCase",
    "toUpperCase",
    "charAt",
    "charCodeAt",
    "codePointAt",
    "substring",
    "substr",
    "startsWith",
    "endsWith",
    "padStart",
    "padEnd",
    "repeat",
    "normalize",
    "localeCompare",
    // Promise methods
    "then",
    "catch",
    "finally",
    // Object/Function methods
    "toString",
    "valueOf",
    "toJSON",
    "hasOwnProperty",
    "call",
    "apply",
    "bind",
];

// Check if a method name is a transformation method that shouldn't be used for naming.
pub fn is_transformation_method(name: &str) -> bool {
    TRANSFORMATION_METHODS.contains(&name)
}

pub const JS_RESERVED_WORDS: &[&str] = &[
    "break",
    "case",
    "catch",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "finally",
    "for",
    "function",
    "if",
    "in",
    "instanceof",
    "new",
    "return",
    "switch",
    "this",
    "throw",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "class",
    "const",
    "enum",
    "export",
    "extends",
    "import",
    "super",
    "implements",
    "interface",
    "let",
    "package",
    "private",
    "protected",
    "public",
    "static",
    "yield",
    "null",
    "true",
    "false",
    "undefined",
    "NaN",
    "Infinity",
    "arguments",
    "eval",
];

pub fn is_reserved_word(name: &str) -> bool {
    JS_RESERVED_WORDS.contains(&name)
}
