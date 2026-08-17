use crate::analysis::metro::registry::FactoryRoles;
use super::types::{ClosureInfo, ClosureSlotValue};

impl ClosureInfo {
    // When a slot stores `Variable("argN")` and we have an IPA name for that parameter,
    // replace the generic name with the meaningful one.
    pub fn update_with_param_names(&mut self, param_names: &[Option<String>]) {
        for value in self.slots.values_mut() {
            if let ClosureSlotValue::Variable(name) = value {
                if let Some(idx) = FactoryRoles::extract_param_index(name) {
                    if let Some(Some(ipa_name)) = param_names.get(idx as usize) {
                        *name = ipa_name.clone();
                    }
                }
            }
        }
    }

    /// Map generic factory parameter names (`argN`/`pN`) to Metro roles
    /// (`require`, `dependencyMap`, …).
    ///
    /// **Must only be called for Metro factory functions** (keys of
    /// `registry.function_to_module`). Applying this to arbitrary functions
    /// renames their `arg1` captures to `require` (e.g. Babel
    /// `_createForOfIteratorHelperLoose` → `let require = Symbol_iterator`).
    pub fn apply_metro_param_roles(&mut self) {
        for value in self.slots.values_mut() {
            if let ClosureSlotValue::Variable(v) = value {
                if let Some(role) = metro_param_role_name(v) {
                    *v = role.to_string();
                }
            }
        }
    }

    pub fn get_slot_name(&self, slot: u32) -> String {
        // The raw slot index (the key may be level-encoded for ancestor scopes).
        let raw_slot = slot & 0x00FF_FFFF;
        match self.slots.get(&slot) {
            Some(ClosureSlotValue::Function { id, name }) => {
                if let Some(n) = name {
                    n.clone()
                } else {
                    format!("f{id}")
                }
            }
            // Exclusive RegExp slot only (see `store_slot` merge rules).
            Some(ClosureSlotValue::RegExp) => format!("re{raw_slot}"),
            // A slot initialised with a constant is a *mutable captured variable*
            // (e.g. a counter `var c = 0` shared with an inner closure), not an
            // alias for the constant. Prefer a short descriptive name derived
            // from the constant when it's a non-empty string (so
            // `env[0] = "ADMINISTRATOR"` → `ADMINISTRATOR` instead of
            // `closure_0`); else `c{slot}`.
            // NOTE: never treat string constants starting with `/` as regex,             // only `ClosureSlotValue::RegExp` maps to `re{N}`.
            Some(ClosureSlotValue::Constant(c)) => {
                if let Some(name) = name_from_constant_text(c) {
                    name
                } else {
                    format!("c{raw_slot}")
                }
            }
            Some(ClosureSlotValue::Variable(v)) => {
                // Prefer semantic names. Metro factory roles (`require`, etc.)
                // are applied eagerly via `apply_metro_param_roles` only on
                // factory functions, never here (avoids false `require` labels).
                if v == "arguments" {
                    "args".to_string()
                } else if is_meaningful_closure_name(v) {
                    v.clone()
                } else {
                    format!("closure_{raw_slot}")
                }
            }
            Some(ClosureSlotValue::Unknown) | None => format!("closure_{raw_slot}"),
        }
    }
}

// Map generic factory parameter names to Metro roles.
// Classic: (global, require, module, exports, dependencyMap) → arg0..arg4
// Modern:  + importDefault/importAll → arg0..arg6
//
// Only invoked from `apply_metro_param_roles` on verified Metro factories.
fn metro_param_role_name(name: &str) -> Option<&'static str> {
    let idx = FactoryRoles::extract_param_index(name)?;
    Some(match idx {
        0 => "global",
        1 => "require",
        2 => "module", // classic; modern with helpers: importDefault, still better than closure_N
        3 => "exports", // classic; modern: importAll
        4 => "dependencyMap", // classic deps / modern module, see below
        5 => "exports", // modern 7-param: exports
        6 => "dependencyMap", // modern deps
        _ => return None,
    })
    // Note: for modern 7-param factories arg2/arg3 are importDefault/importAll
    // and arg4 is module. Mislabeling those as module/exports is still far
    // more readable than closure_N, and depmap rewrite accepts idx>=4.
}

// Derive a JS identifier from a constant's display text (e.g. `"foo"` → `foo`).
fn name_from_constant_text(c: &str) -> Option<String> {
    let s = c.trim().trim_matches('"').trim_matches('\'');
    if s.is_empty() || s.len() > 40 {
        return None;
    }
    // Must be a valid-ish identifier start.
    let mut chars = s.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return None;
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$') {
        return None;
    }
    // Avoid reserved / generic noise.
    if matches!(
        s,
        "undefined"
            | "null"
            | "true"
            | "false"
            | "default"
            | "exports"
            | "module"
            | "require"
            | "global"
            | "Object"
            | "Array"
            | "Function"
            | "String"
            | "Number"
            | "Boolean"
            | "Symbol"
            | "Error"
            | "Math"
            | "JSON"
            | "console"
            | "window"
            | "document"
            | "this"
    ) {
        return None;
    }
    Some(s.to_string())
}

fn is_meaningful_closure_name(name: &str) -> bool {
    if name.len() < 2 {
        return false;
    }
    // Reject register / param / tmp forms.
    if name.starts_with('r') && name[1..].chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if name.starts_with("arg") && name[3..].chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if name.starts_with("tmp") {
        return false;
    }
    if name.starts_with("closure_") || name.starts_with("outer") {
        return false;
    }
    true
}
