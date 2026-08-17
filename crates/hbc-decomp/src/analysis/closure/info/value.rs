use crate::ir::{Expression, Value};
use std::collections::BTreeMap;
use super::types::ClosureSlotValue;

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
