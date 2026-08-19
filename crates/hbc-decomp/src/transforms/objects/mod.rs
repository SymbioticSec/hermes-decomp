use crate::ir::{AssignTarget, Expression, ObjectProperty, PropertyKey, Statement, Value,
    expr_uses_register, stmt_has_side_effects};
use std::collections::HashSet;

mod inline_literals;
mod slot_fills;

pub use slot_fills::fold_slot_index_fills;
use inline_literals::inline_single_use_literals;

#[cfg(test)]
mod tests;

pub fn transform_object_literals(statements: &mut Vec<Statement>) {
    // HBC ≥97 emits a shape-table object literal with placeholder values for
    // non-serializable properties (`{a:1, b:null}`), then fills them via
    // `PutOwnBySlotIdx obj, val, slot`, which lowers to `obj[slot] = val`. Fold
    // those slot fills back into the literal's Nth property before the rest of
    // the object-literal handling runs.
    fold_slot_index_fills(statements);

    // A register assigned more than once in the whole body is a genuine
    // re-assignment: referencing it as a property value is unsafe because its
    // value may differ at the fold point. A register defined exactly once (the
    // common case for nested object construction, `r1 = {}; r1.c = 42` then
    // `r0.b = r1`) is safe to reference; it will be inlined later. (The previous
    // forward-tracking wrongly counted an inner object's own definition as a
    // reassignment, blocking nested `{a:{b:{c:42}}}` reconstruction.)
    let multi_assigned = registers_assigned_multiple_times(statements);

    let mut i = 0;
    while i < statements.len() {
        // Look for: let obj_reg = NewObject(parent);
        if let Some((obj_reg, _)) = is_new_object(&statements[i]) {
            // Collect properties
            let mut properties = Vec::new();
            let mut j = i + 1;
            let mut consumed_indices = Vec::new();

            while j < statements.len() {
                let stmt = &statements[j];

                if is_put_prop(stmt, obj_reg, &mut properties) {
                    let prop = match properties.last() {
                        Some(p) => p,
                        None => break,
                    };
                    if value_uses_any_reg(&prop.value, &multi_assigned) {
                        // Value references a re-assigned register: unsafe to fold.
                        properties.pop();
                        break;
                    }
                    consumed_indices.push(j);
                } else if is_reg_used(stmt, obj_reg) || is_reg_assigned(stmt, obj_reg) {
                    // Block boundary
                    break;
                } else if stmt_has_side_effects(stmt) {
                    // Stop on any statement with side effects for safety
                    break;
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

    // Hermes constructs nested objects outer-first (`r4={}; r0={}; r1={}; ...`)
    // then populates them inner-first, so after folding the literals reference
    // registers defined *later* (`r4 = {a:r0}` before `r0 = {b:r1}`). Inline
    // single-use, single-def pure object/array literals into their use site so
    // `{a:{b:{c:42}}}` is reconstructed (order-independent, these values are pure).
    inline_single_use_literals(statements);
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

pub(super) fn is_reg_used(stmt: &Statement, reg: u32) -> bool {
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

// Registers that are the target of a register assignment more than once across
// the whole body (recursively). These are genuine re-assignments whose value is
// unsafe to capture into a folded object literal.
fn registers_assigned_multiple_times(stmts: &[Statement]) -> HashSet<u32> {
    let mut counts = std::collections::HashMap::new();
    count_register_assigns(stmts, &mut counts);
    counts
        .into_iter()
        .filter(|(_, c)| *c >= 2)
        .map(|(r, _)| r)
        .collect()
}

fn count_register_assigns(stmts: &[Statement], counts: &mut std::collections::HashMap<u32, usize>) {
    for stmt in stmts {
        if let Statement::Assign { target: AssignTarget::Register(r), .. } = stmt {
            *counts.entry(*r).or_insert(0) += 1;
        }
        match stmt {
            Statement::If { then_body, else_body, .. } => {
                count_register_assigns(then_body, counts);
                count_register_assigns(else_body, counts);
            }
            Statement::While { body, .. }
            | Statement::DoWhile { body, .. }
            | Statement::For { body, .. }
            | Statement::ForIn { body, .. }
            | Statement::ForOf { body, .. } => count_register_assigns(body, counts),
            Statement::Block(inner) => count_register_assigns(inner, counts),
            Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
                count_register_assigns(try_body, counts);
                count_register_assigns(catch_body, counts);
                count_register_assigns(finally_body, counts);
            }
            Statement::Switch { cases, default, .. } => {
                for (_, body) in cases {
                    count_register_assigns(body, counts);
                }
                if let Some(d) = default {
                    count_register_assigns(d, counts);
                }
            }
            _ => {}
        }
    }
}
