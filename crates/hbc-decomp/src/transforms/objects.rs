use crate::ir::{AssignTarget, Expression, ObjectProperty, PropertyKey, Statement, Value,
    expr_uses_register, stmt_has_side_effects};
use std::collections::HashSet;

pub fn transform_object_literals(statements: &mut Vec<Statement>) {
    // HBC ≥97 emits a shape-table object literal with placeholder values for
    // non-serializable properties (`{a:1, b:null}`), then fills them via
    // `PutOwnBySlotIdx obj, val, slot` — which lowers to `obj[slot] = val`. Fold
    // those slot fills back into the literal's Nth property before the rest of
    // the object-literal handling runs.
    fold_slot_index_fills(statements);

    // A register assigned more than once in the whole body is a genuine
    // re-assignment: referencing it as a property value is unsafe because its
    // value may differ at the fold point. A register defined exactly once (the
    // common case for nested object construction — `r1 = {}; r1.c = 42` then
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
    // `{a:{b:{c:42}}}` is reconstructed (order-independent — these values are pure).
    inline_single_use_literals(statements);
}

// Fold `obj = { k0:v0, k1:<placeholder>, ... }; obj[N] = val` (a slot-index
// fill from PutOwnBySlotIdx) into the literal's Nth property. Only replaces a
// placeholder value (null/undefined/empty), which is what the shape-table form
// leaves for non-serializable property values — so a genuine numeric-key write
// is never absorbed.
fn fold_slot_index_fills(statements: &mut Vec<Statement>) {
    let mut i = 0;
    while i < statements.len() {
        let prop_count = match &statements[i] {
            Statement::Assign {
                target: AssignTarget::Register(_),
                value: Expression::Object { properties },
            } if !properties.is_empty() => properties.len(),
            _ => {
                i += 1;
                continue;
            }
        };
        let obj_reg = match &statements[i] {
            Statement::Assign { target: AssignTarget::Register(r), .. } => *r,
            _ => unreachable!(),
        };

        let mut j = i + 1;
        let mut remove = Vec::new();
        while j < statements.len() {
            if let Some((slot, val)) = slot_index_fill(&statements[j], obj_reg, prop_count) {
                // Replace the placeholder at `slot` in the literal at `i`.
                if let Statement::Assign { value: Expression::Object { properties }, .. } =
                    &mut statements[i]
                {
                    if is_placeholder(&properties[slot].value) {
                        properties[slot].value = val;
                        remove.push(j);
                        j += 1;
                        continue;
                    }
                }
                break;
            } else if is_reg_used(&statements[j], obj_reg)
                || is_reg_assigned(&statements[j], obj_reg)
                || stmt_has_side_effects(&statements[j])
            {
                break;
            }
            j += 1;
        }
        for &idx in remove.iter().rev() {
            statements.remove(idx);
        }
        i += 1;
    }
}

// `obj[N] = val` with a constant N < prop_count → (N, val).
fn slot_index_fill(stmt: &Statement, obj_reg: u32, prop_count: usize) -> Option<(usize, Expression)> {
    if let Statement::Assign {
        target: AssignTarget::Index { object: Expression::Value(Value::Register(r)), key },
        value,
    } = stmt
    {
        if *r != obj_reg {
            return None;
        }
        let n = match key {
            Expression::Value(Value::Constant(crate::ir::Constant::Integer(n))) if *n >= 0 => {
                *n as usize
            }
            _ => return None,
        };
        if n < prop_count {
            return Some((n, value.clone()));
        }
    }
    None
}

fn is_placeholder(expr: &Expression) -> bool {
    matches!(
        expr,
        Expression::Value(Value::Constant(
            crate::ir::Constant::Null | crate::ir::Constant::Undefined
        ))
    )
}

// Inline registers defined once as a pure object/array literal and used exactly
// once, regardless of statement order. Repeats to a fixed point so deep nests
// collapse fully.
fn inline_single_use_literals(statements: &mut Vec<Statement>) {
    loop {
        let mut def_count: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        let mut use_count: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        for stmt in statements.iter() {
            if let Statement::Assign { target: AssignTarget::Register(r), .. } = stmt {
                *def_count.entry(*r).or_insert(0) += 1;
            }
            collect_value_reg_uses(stmt, &mut use_count);
        }

        // Find a register: defined once as a pure object/array literal, used
        // exactly once. Restricted to composite literals (not bare constants /
        // register copies) — those are handled by the general inliner and folding
        // them here would wrongly substitute e.g. an accumulator's init `0` into a
        // later `return`.
        let mut chosen: Option<(u32, Expression)> = None;
        for stmt in statements.iter() {
            if let Statement::Assign { target: AssignTarget::Register(r), value } = stmt {
                let is_composite =
                    matches!(value, Expression::Object { .. } | Expression::Array { .. });
                if is_composite
                    && def_count.get(r) == Some(&1)
                    && use_count.get(r) == Some(&1)
                    && is_pure_literal(value)
                {
                    chosen = Some((*r, value.clone()));
                    break;
                }
            }
        }

        let Some((reg, value)) = chosen else { break };
        // Substitute the value into its single use, then drop the definition.
        for stmt in statements.iter_mut() {
            substitute_register_in_stmt(stmt, reg, &value);
        }
        statements.retain(|stmt| {
            !matches!(stmt, Statement::Assign { target: AssignTarget::Register(r), .. } if *r == reg)
        });
    }
}

fn is_pure_literal(expr: &Expression) -> bool {
    match expr {
        Expression::Object { properties } => properties.iter().all(|p| is_pure_literal(&p.value)),
        Expression::Array { elements } => elements.iter().flatten().all(is_pure_literal),
        Expression::Value(Value::Constant(_)) => true,
        Expression::Value(Value::Register(_)) => true,
        _ => false,
    }
}

fn collect_value_reg_uses(stmt: &Statement, counts: &mut std::collections::HashMap<u32, usize>) {
    use crate::ir::Visitor;
    struct C<'a>(&'a mut std::collections::HashMap<u32, usize>);
    impl<'a, 'b> Visitor<'b> for C<'a> {
        fn visit_assign_target(&mut self, target: &'b AssignTarget) {
            // Count register reads that occur inside a member/index target
            // (e.g. `r0.b = ...` reads r0), but NOT the plain register def.
            match target {
                AssignTarget::Member { object, .. } => self.visit_expression(object),
                AssignTarget::Index { object, key } => {
                    self.visit_expression(object);
                    self.visit_expression(key);
                }
                _ => {}
            }
        }
        fn visit_expression(&mut self, e: &'b Expression) {
            if let Expression::Value(Value::Register(r)) = e {
                *self.0.entry(*r).or_insert(0) += 1;
            }
            self.walk_expression(e);
        }
    }
    C(counts).visit_statement(stmt);
}

fn substitute_register_in_stmt(stmt: &mut Statement, reg: u32, value: &Expression) {
    use crate::ir::MutVisitor;
    struct S<'a> {
        reg: u32,
        value: &'a Expression,
    }
    impl<'a> MutVisitor for S<'a> {
        fn visit_expression(&mut self, e: &mut Expression) {
            if let Expression::Value(Value::Register(r)) = e {
                if *r == self.reg {
                    *e = self.value.clone();
                    return;
                }
            }
            self.walk_expression(e);
        }
    }
    S { reg, value }.visit_statement(stmt);
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
