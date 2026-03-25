use crate::ir::{AssignTarget, Expression, Statement, Value};
use crate::analysis::metro::registry::FactoryRoles;
use std::collections::BTreeMap;

// Encode level and slot into a single u32 key for HashMap storage.
// Uses high 8 bits for level, low 24 bits for slot.
pub fn encode_level_slot(level: u32, slot: u32) -> u32 {
    ((level & 0xFF) << 24) | (slot & 0xFFFFFF)
}

#[derive(Debug, Clone)]
pub enum ClosureSlotValue {
    Function { id: u32, name: Option<String> },
    Constant(String),
    Variable(String),
    Unknown,
}

#[derive(Debug, Clone)]
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
                    if let Some(val) = value_from_expr(value, None, false) {
                        reg_values.insert(*r, val);
                    }
                }

                if let AssignTarget::ClosureVar { slot, .. } = target {
                    if let Some(val) = value_from_expr(value, Some(reg_values), false) {
                        self.slots.insert(*slot, val);
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
            Statement::While { body, .. } => {
                for s in body {
                    self.analyze_stmt(s, reg_values);
                }
            }
            Statement::Block(inner) => {
                for s in inner {
                    self.analyze_stmt(s, reg_values);
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

    pub fn get_slot_name(&self, slot: u32) -> String {
        match self.slots.get(&slot) {
            Some(ClosureSlotValue::Function { id, name }) => {
                if let Some(n) = name {
                    n.clone()
                } else {
                    format!("f{id}")
                }
            }
            Some(ClosureSlotValue::Constant(c)) => c.clone(),
            Some(ClosureSlotValue::Variable(v)) => v.clone(),
            Some(ClosureSlotValue::Unknown) | None => format!("closure_{slot}"),
        }
    }
}

// This is the canonical implementation used by both `ClosureInfo::analyze` and
// `ClosureContext::analyze_stmt_context`.
//
// - `reg_values: Some(map)` — resolve registers via the map, return `Unknown` for unresolvable.
// - `reg_values: None` — don't resolve registers, return `None` for unresolvable.
// - `resolve_members: false` — basic extraction (Function, Register, Constant, Variable, Parameter).
// - `resolve_members: true` — extended: also handles `This → "self"`, `.default` member access,
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
