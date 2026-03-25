use crate::ir::{AssignTarget, Expression, ObjectProperty, PropertyKey, Statement, Value,
    expr_uses_register, stmt_has_side_effects};
use std::collections::HashSet;

pub fn transform_object_literals(statements: &mut Vec<Statement>) {
    let mut i = 0;
    while i < statements.len() {
        // Look for: let obj_reg = NewObject(parent);
        if let Some((obj_reg, _)) = is_new_object(&statements[i]) {
            // Collect properties
            let mut properties = Vec::new();
            let mut j = i + 1;
            let mut consumed_indices = Vec::new();
            // Track registers that get reassigned between the NewObject and property assignments.
            // If a property value references a reassigned register, we must stop folding
            // because the value no longer refers to what it did at the point of the fold.
            let mut reassigned_regs: HashSet<u32> = HashSet::new();

            while j < statements.len() {
                let stmt = &statements[j];

                if is_put_prop(stmt, obj_reg, &mut properties) {
                    // Check if the property value references any register that was reassigned
                    let prop = match properties.last() {
                        Some(p) => p,
                        None => break,
                    };
                    if value_uses_any_reg(&prop.value, &reassigned_regs) {
                        // Undo: remove the property we just added, stop folding
                        properties.pop();
                        break;
                    }
                    consumed_indices.push(j);
                } else if is_reg_used(stmt, obj_reg) || is_reg_assigned(stmt, obj_reg) {
                    // Block boundary
                    break;
                } else {
                    // Track register reassignments
                    if let Some(assigned_reg) = get_assigned_register(stmt) {
                        reassigned_regs.insert(assigned_reg);
                    }
                    // Stop on any statement with side effects for safety
                    if stmt_has_side_effects(stmt) {
                        break;
                    }
                }
                j += 1;
            }

            if !properties.is_empty() {
                // Replace the NewObject call
                if let Statement::Assign { target, .. } = &mut statements[i] {
                    *target = AssignTarget::Register(obj_reg);
                    statements[i] = Statement::Assign {
                        target: AssignTarget::Register(obj_reg),
                        value: Expression::Object { properties },
                    };

                    for &idx in consumed_indices.iter().rev() {
                        statements.remove(idx);
                    }

                    i += 1;
                    continue;
                }
            }
        }
        i += 1;
    }
}

fn is_new_object(stmt: &Statement) -> Option<(u32, usize)> {
    if let Statement::Assign {
        target: AssignTarget::Register(r),
        value: Expression::New { .. },
    } = stmt
    {
        return Some((*r, 0));
    }
    if let Statement::Assign {
        target: AssignTarget::Register(r),
        value: Expression::Object { properties },
    } = stmt
    {
        if properties.is_empty() {
            return Some((*r, 0));
        }
    }
    if let Statement::Assign {
        target: AssignTarget::Register(r),
        value: Expression::Unknown { opcode, .. },
    } = stmt
    {
        if opcode == "NewObject" || opcode == "NewObjectWithBuffer" {
            return Some((*r, 0));
        }
    }

    None
}

fn is_put_prop(stmt: &Statement, obj_reg: u32, props: &mut Vec<ObjectProperty>) -> bool {
    // Correct struct pattern for Member variant (property is String)
    if let Statement::Assign {
        target:
            AssignTarget::Member {
                object: Expression::Value(Value::Register(r)),
                property,
            },
        value,
    } = stmt
    {
        if *r == obj_reg {
            props.push(ObjectProperty {
                key: PropertyKey::Ident(property.clone()),
                value: value.clone(),
            });
            return true;
        }
    }
    // Also check Index (computed)
    if let Statement::Assign {
        target:
            AssignTarget::Index {
                object: Expression::Value(Value::Register(r)),
                key,
            },
        value,
    } = stmt
    {
        if *r == obj_reg {
            props.push(ObjectProperty {
                key: PropertyKey::Computed(Box::new(key.clone())),
                value: value.clone(),
            });
            return true;
        }
    }

    // Check Unknown opcodes
    if let Statement::Expr(Expression::Unknown { opcode, .. }) = stmt {
        if opcode == "PutById" {
            // Ignored
        }
    }

    false
}

fn is_reg_assigned(stmt: &Statement, reg: u32) -> bool {
    match stmt {
        Statement::Assign {
            target: AssignTarget::Register(r),
            ..
        } => *r == reg,
        _ => false,
    }
}

fn is_reg_used(stmt: &Statement, reg: u32) -> bool {
    match stmt {
        Statement::Assign { target, value } => {
            let target_uses = match target {
                AssignTarget::Member { object, .. } => expr_uses_register(object, reg),
                AssignTarget::Index { object, key } => {
                    expr_uses_register(object, reg) || expr_uses_register(key, reg)
                }
                _ => false,
            };
            target_uses || expr_uses_register(value, reg)
        }
        Statement::Expr(e) => expr_uses_register(e, reg),
        Statement::Return(Some(e)) | Statement::Throw(e) => expr_uses_register(e, reg),
        Statement::If { condition, .. } => expr_uses_register(condition, reg),
        Statement::While { condition, .. } => expr_uses_register(condition, reg),
        _ => false,
    }
}

// Check if an expression references any register from a set of reassigned registers.
fn value_uses_any_reg(expr: &Expression, regs: &HashSet<u32>) -> bool {
    if regs.is_empty() {
        return false;
    }
    match expr {
        Expression::Value(Value::Register(r)) => regs.contains(r),
        Expression::Binary { left, right, .. } => {
            value_uses_any_reg(left, regs) || value_uses_any_reg(right, regs)
        }
        Expression::Unary { operand, .. } => value_uses_any_reg(operand, regs),
        Expression::Member {
            object, property, ..
        } => {
            value_uses_any_reg(object, regs)
                || match property {
                    PropertyKey::Computed(k) => value_uses_any_reg(k, regs),
                    _ => false,
                }
        }
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            value_uses_any_reg(callee, regs) || arguments.iter().any(|a| value_uses_any_reg(a, regs))
        }
        Expression::Object { properties } => properties
            .iter()
            .any(|p| value_uses_any_reg(&p.value, regs)),
        Expression::Array { elements } => elements.iter().flatten().any(|e| value_uses_any_reg(e, regs)),
        _ => false,
    }
}

// Extract the register being assigned by a statement, if any.
fn get_assigned_register(stmt: &Statement) -> Option<u32> {
    match stmt {
        Statement::Assign {
            target: AssignTarget::Register(r),
            ..
        } => Some(*r),
        _ => None,
    }
}
