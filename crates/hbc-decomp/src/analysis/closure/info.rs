use crate::ir::{AssignTarget, Expression, Statement, Value};
use crate::analysis::metro::registry::FactoryRoles;
use std::collections::BTreeMap;

// Encode level and slot into a single u32 key for HashMap storage.
// Uses high 8 bits for level, low 24 bits for slot.
pub fn encode_level_slot(level: u32, slot: u32) -> u32 {
    ((level & 0xFF) << 24) | (slot & 0xFFFFFF)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ClosureSlotValue {
    Function { id: u32, name: Option<String> },
    Constant(String),
    /// Slot exclusively holds a RegExp literal (no non-regex stores observed).
    /// Only this variant is named `re{N}`, string constants starting with `/`
    /// and reused env slots must not look like regexes.
    RegExp,
    Variable(String),
    Unknown,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClosureInfo {
    pub slots: BTreeMap<u32, ClosureSlotValue>,
}

impl Default for ClosureInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl ClosureInfo {
    pub fn new() -> Self {
        Self {
            slots: BTreeMap::new(),
        }
    }

    /// Record a store into an env slot with reuse-aware merge.
    ///
    /// Hermes reuses environment slots aggressively. A slot that once held a
    /// regex and later holds `Math.random` must not keep the `re{N}` name for
    /// every use (flow-insensitive naming would otherwise mislabel).
    ///
    /// Mutable captured bindings are initialised once (often with a Constant)
    /// then updated with temps (`sum = c0 + 1; c0 = sum`). Flow-insensitive
    /// last-write would rename the *slot* to `sum` and turn the update into
    /// `sum = sum + 1` (TDZ). Keep the first stable name for the slot.
    pub fn store_slot(&mut self, slot: u32, val: ClosureSlotValue) {
        let next = match self.slots.get(&slot) {
            None => val,
            Some(ClosureSlotValue::RegExp) => match val {
                ClosureSlotValue::RegExp => ClosureSlotValue::RegExp,
                other => other,
            },
            Some(prev) => match &val {
                // Non-regex then regex ⇒ slot reuse; drop RegExp so we never
                // emit `re{N}` for mixed slots.
                ClosureSlotValue::RegExp => ClosureSlotValue::Unknown,
                // Temp / intermediate Variable must not rename a stable slot.
                ClosureSlotValue::Variable(v) if is_ephemeral_slot_name(v) => {
                    // Prefer an existing Constant/Function/stable Variable name.
                    if slot_name_is_stable(prev) {
                        prev.clone()
                    } else {
                        val
                    }
                }
                other => other.clone(),
            },
        };
        self.slots.insert(slot, next);
    }

    pub fn analyze(stmts: &[Statement]) -> Self {
        let mut info = Self::new();
        let mut register_values: BTreeMap<u32, ClosureSlotValue> = BTreeMap::new();

        for stmt in stmts {
            info.analyze_stmt(stmt, &mut register_values);
        }

        info
    }

    fn analyze_stmt(&mut self, stmt: &Statement, reg_values: &mut BTreeMap<u32, ClosureSlotValue>) {
        match stmt {
            Statement::Assign { target, value } => {
                if let AssignTarget::Register(r) = target {
                    // Use reg_values so copies like `r5 = require` still track.
                    if let Some(val) = value_from_expr(value, Some(reg_values), true) {
                        reg_values.insert(*r, val);
                    }
                }

                if let AssignTarget::ClosureVar { slot, .. } = target {
                    if let Some(val) = value_from_expr(value, Some(reg_values), true) {
                        self.store_slot(*slot, val);
                    }
                }
            }
            Statement::If {
                then_body,
                else_body,
                ..
            } => {
                for s in then_body {
                    self.analyze_stmt(s, reg_values);
                }
                for s in else_body {
                    self.analyze_stmt(s, reg_values);
                }
            }
            Statement::While { body, .. }
            | Statement::DoWhile { body, .. }
            | Statement::For { body, .. }
            | Statement::ForIn { body, .. }
            | Statement::ForOf { body, .. }
            | Statement::Block(body) => {
                for s in body {
                    self.analyze_stmt(s, reg_values);
                }
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                finally_body,
                ..
            } => {
                for s in try_body {
                    self.analyze_stmt(s, reg_values);
                }
                for s in catch_body {
                    self.analyze_stmt(s, reg_values);
                }
                for s in finally_body {
                    self.analyze_stmt(s, reg_values);
                }
            }
            Statement::Switch { cases, default, .. } => {
                for (_, body) in cases {
                    for s in body {
                        self.analyze_stmt(s, reg_values);
                    }
                }
                if let Some(d) = default {
                    for s in d {
                        self.analyze_stmt(s, reg_values);
                    }
                }
            }
            _ => {}
        }
    }

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

/// Names that are intermediate SSA-like temps, not the identity of a captured binding.
fn is_ephemeral_slot_name(name: &str) -> bool {
    if name == "tmp"
        || name.strip_prefix("tmp").is_some_and(|s| s.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    if name.strip_prefix('r').is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    // Common names inferred from binary ops / short-lived results.
    matches!(
        name,
        "sum" | "diff" | "product" | "quotient" | "text" | "result" | "value" | "ret"
            | "tmpResult" | "callResult"
    ) || name.ends_with("Result")
        || name.ends_with("Return")
}

fn slot_name_is_stable(val: &ClosureSlotValue) -> bool {
    match val {
        ClosureSlotValue::Constant(_) | ClosureSlotValue::Function { .. } => true,
        ClosureSlotValue::Variable(v) => !is_ephemeral_slot_name(v),
        ClosureSlotValue::RegExp | ClosureSlotValue::Unknown => false,
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
    if name.starts_with("closure_") {
        return false;
    }
    true
}

// This is the canonical implementation used by both `ClosureInfo::analyze` and
// `ClosureContext::analyze_stmt_context`.
//
// - `reg_values: Some(map)`, resolve registers via the map, return `Unknown` for unresolvable.
// - `reg_values: None`, don't resolve registers, return `None` for unresolvable.
// - `resolve_members: false`, basic extraction (Function, Register, Constant, Variable, Parameter).
// - `resolve_members: true`, extended: also handles `This → "self"`, `.default` member access,
//   and generic property access (property name ≤ 25 chars, excluding "prototype"/"exports"/"__esModule").
pub fn value_from_expr(
    expr: &Expression,
    reg_values: Option<&BTreeMap<u32, ClosureSlotValue>>,
    resolve_members: bool,
) -> Option<ClosureSlotValue> {
    match expr {
        Expression::Function { id, name, .. } => Some(ClosureSlotValue::Function {
            id: id.0,
            name: name.clone(),
        }),
        Expression::RegExp { .. } => {
            // Dedicated variant, only exclusive-RegExp slots become re{N}.
            Some(ClosureSlotValue::RegExp)
        }
        Expression::Value(Value::Register(r)) => {
            reg_values.and_then(|rv| rv.get(r).cloned())
        }
        Expression::Value(Value::Constant(c)) => {
            Some(ClosureSlotValue::Constant(format!("{c}")))
        }
        Expression::Value(Value::Variable(name)) => {
            Some(ClosureSlotValue::Variable(name.clone()))
        }
        Expression::Value(Value::Parameter(i)) => {
            Some(ClosureSlotValue::Variable(format!("arg{i}")))
        }
        Expression::Value(Value::This) if resolve_members => {
            Some(ClosureSlotValue::Variable("self".to_string()))
        }
        Expression::Member { object, property, .. } if resolve_members => {
            if let Some(prop) = ident_from_property_key(property) {
                if prop == "default" {
                    match &**object {
                        Expression::Value(Value::Variable(name)) => {
                            return Some(ClosureSlotValue::Variable(name.clone()));
                        }
                        Expression::Value(Value::Register(r)) => {
                            if let Some(rv) = reg_values {
                                if let Some(ClosureSlotValue::Variable(name)) = rv.get(r) {
                                    return Some(ClosureSlotValue::Variable(name.clone()));
                                }
                            }
                            return if reg_values.is_some() {
                                Some(ClosureSlotValue::Unknown)
                            } else {
                                None
                            };
                        }
                        _ => {
                            return if reg_values.is_some() {
                                Some(ClosureSlotValue::Unknown)
                            } else {
                                None
                            };
                        }
                    }
                } else if !prop.is_empty() && prop.len() <= 25
                    && prop != "prototype" && prop != "exports" && prop != "__esModule"
                {
                    return Some(ClosureSlotValue::Variable(prop));
                }
            }
            if reg_values.is_some() {
                Some(ClosureSlotValue::Unknown)
            } else {
                None
            }
        }
        _ => {
            if reg_values.is_some() {
                Some(ClosureSlotValue::Unknown)
            } else {
                None
            }
        }
    }
}

pub fn ident_from_property_key(prop: &crate::ir::PropertyKey) -> Option<String> {
    match prop {
        crate::ir::PropertyKey::Ident(name) | crate::ir::PropertyKey::String(name) => {
            Some(name.clone())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metro_roles_not_applied_in_get_slot_name() {
        let mut info = ClosureInfo::new();
        info.slots
            .insert(1, ClosureSlotValue::Variable("arg1".into()));
        // Without apply_metro_param_roles, arg1 stays generic (not "require").
        assert_eq!(info.get_slot_name(1), "closure_1");
    }

    #[test]
    fn metro_roles_applied_only_via_explicit_call() {
        let mut info = ClosureInfo::new();
        info.slots
            .insert(1, ClosureSlotValue::Variable("arg1".into()));
        info.slots
            .insert(4, ClosureSlotValue::Variable("arg4".into()));
        info.apply_metro_param_roles();
        assert_eq!(info.get_slot_name(1), "require");
        assert_eq!(info.get_slot_name(4), "dependencyMap");
    }

    #[test]
    fn re_n_only_for_exclusive_regexp_slots() {
        let mut info = ClosureInfo::new();
        info.store_slot(3, ClosureSlotValue::RegExp);
        assert_eq!(info.get_slot_name(3), "re3");

        // Non-regex store overwrites RegExp → no reN.
        info.store_slot(3, ClosureSlotValue::Variable("parseInt".into()));
        assert_eq!(info.get_slot_name(3), "parseInt");
    }

    #[test]
    fn re_n_dropped_when_regex_follows_non_regex() {
        let mut info = ClosureInfo::new();
        info.store_slot(5, ClosureSlotValue::Variable("tmp".into()));
        info.store_slot(5, ClosureSlotValue::RegExp);
        // Mixed reuse → Unknown → closure_N, not re5.
        assert_eq!(info.get_slot_name(5), "closure_5");
    }

    #[test]
    fn string_constant_slash_is_not_ren() {
        let mut info = ClosureInfo::new();
        info.store_slot(0, ClosureSlotValue::Constant("\"/api/v1\"".into()));
        assert_eq!(info.get_slot_name(0), "c0");
    }

    #[test]
    fn mutable_counter_keeps_init_name_not_sum() {
        // env[0] = 0; env[0] = sum  → still named c0, not sum
        let mut info = ClosureInfo::new();
        info.store_slot(0, ClosureSlotValue::Constant("0".into()));
        info.store_slot(0, ClosureSlotValue::Variable("sum".into()));
        assert_eq!(info.get_slot_name(0), "c0");
    }
}
