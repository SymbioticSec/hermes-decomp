use crate::ir::{AssignTarget, Expression, ObjectProperty, PropertyKey, Statement, Value,
    expr_uses_register, stmt_has_side_effects};
use std::collections::HashSet;

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

// Fold `obj = { k0:v0, k1:<placeholder>, ... }; obj[N] = val` (a slot-index
// fill from PutOwnBySlotIdx) into the literal's Nth property. Only replaces a
// placeholder value (null/undefined/empty), which is what the shape-table form
// leaves for non-serializable property values, so a genuine numeric-key write
// is never absorbed. Matches both still-register objects and named
// Variable/Let objects (after register naming).
pub fn fold_slot_index_fills(statements: &mut Vec<Statement>) {
    // Mark consumed fills and drop them all in one pass. Removing each fill inside
    // the loop shifts the tail on every fill, which is O(n^2) on a large function.
    let mut consumed = vec![false; statements.len()];
    let mut i = 0;
    while i < statements.len() {
        let Some((obj, prop_count)) = object_literal_def(&statements[i]) else {
            i += 1;
            continue;
        };

        let mut j = i + 1;
        while j < statements.len() {
            if let Some((slot, val)) = slot_index_fill(&statements[j], &obj, prop_count) {
                if let Some(properties) = object_properties_mut(&mut statements[i]) {
                    if is_placeholder(&properties[slot].value) {
                        properties[slot].value = val;
                        consumed[j] = true;
                        j += 1;
                        continue;
                    }
                }
                break;
            } else if obj_reassigned(&statements[j], &obj)
                || obj_used(&statements[j], &obj)
                || stmt_has_side_effects(&statements[j])
            {
                break;
            }
            j += 1;
        }
        i += 1;
    }
    if consumed.iter().any(|&c| c) {
        let mut idx = 0;
        statements.retain(|_| {
            let keep = !consumed[idx];
            idx += 1;
            keep
        });
    }
}

enum ObjRef {
    Register(u32),
    Name(String),
}

fn object_literal_def(stmt: &Statement) -> Option<(ObjRef, usize)> {
    match stmt {
        Statement::Assign {
            target: AssignTarget::Register(r),
            value: Expression::Object { properties },
        } if !properties.is_empty() => Some((ObjRef::Register(*r), properties.len())),
        Statement::Assign {
            target: AssignTarget::Variable(name),
            value: Expression::Object { properties },
        } if !properties.is_empty() => Some((ObjRef::Name(name.clone()), properties.len())),
        Statement::Let {
            name,
            value: Expression::Object { properties },
            ..
        } if !properties.is_empty() => Some((ObjRef::Name(name.clone()), properties.len())),
        _ => None,
    }
}

fn object_properties_mut(stmt: &mut Statement) -> Option<&mut Vec<ObjectProperty>> {
    match stmt {
        Statement::Assign {
            value: Expression::Object { properties },
            ..
        }
        | Statement::Let {
            value: Expression::Object { properties },
            ..
        } => Some(properties),
        _ => None,
    }
}

// `obj[N] = val` with a constant N < prop_count → (N, val).
fn slot_index_fill(stmt: &Statement, obj: &ObjRef, prop_count: usize) -> Option<(usize, Expression)> {
    let Statement::Assign {
        target: AssignTarget::Index { object, key },
        value,
    } = stmt
    else {
        return None;
    };
    let matches_obj = match (obj, object) {
        (ObjRef::Register(r), Expression::Value(Value::Register(r2))) => r == r2,
        (ObjRef::Name(n), Expression::Value(Value::Variable(n2))) => n == n2,
        _ => false,
    };
    if !matches_obj {
        return None;
    }
    let n = match key {
        Expression::Value(Value::Constant(crate::ir::Constant::Integer(n))) if *n >= 0 => {
            *n as usize
        }
        _ => return None,
    };
    if n < prop_count {
        Some((n, value.clone()))
    } else {
        None
    }
}

fn obj_reassigned(stmt: &Statement, obj: &ObjRef) -> bool {
    match (obj, stmt) {
        (
            ObjRef::Register(r),
            Statement::Assign {
                target: AssignTarget::Register(r2),
                ..
            },
        ) => r == r2,
        (
            ObjRef::Name(n),
            Statement::Assign {
                target: AssignTarget::Variable(n2),
                ..
            },
        ) => n == n2,
        (ObjRef::Name(n), Statement::Let { name, .. }) => name == n,
        _ => false,
    }
}

fn obj_used(stmt: &Statement, obj: &ObjRef) -> bool {
    match obj {
        ObjRef::Register(r) => is_reg_used(stmt, *r),
        ObjRef::Name(n) => stmt_uses_var(stmt, n),
    }
}

fn stmt_uses_var(stmt: &Statement, name: &str) -> bool {
    match stmt {
        Statement::Assign { target, value } => {
            target_uses_var(target, name) || expr_uses_var(value, name)
        }
        Statement::Let { value, .. } => expr_uses_var(value, name),
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
            expr_uses_var(e, name)
        }
        Statement::If { condition, .. }
        | Statement::While { condition, .. }
        | Statement::DoWhile { condition, .. } => expr_uses_var(condition, name),
        _ => false,
    }
}

fn target_uses_var(target: &AssignTarget, name: &str) -> bool {
    match target {
        AssignTarget::Member { object, .. } => expr_uses_var(object, name),
        AssignTarget::Index { object, key } => {
            expr_uses_var(object, name) || expr_uses_var(key, name)
        }
        _ => false,
    }
}

fn expr_uses_var(expr: &Expression, name: &str) -> bool {
    use crate::ir::Visitor;
    struct C<'a>(&'a str, bool);
    impl Visitor<'_> for C<'_> {
        fn visit_expression(&mut self, e: &Expression) {
            if let Expression::Value(Value::Variable(n)) = e {
                if n == self.0 {
                    self.1 = true;
                    return;
                }
            }
            if !self.1 {
                self.walk_expression(e);
            }
        }
    }
    let mut c = C(name, false);
    c.visit_expression(expr);
    c.1
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
    use std::collections::HashMap;
    // Inlining a single-use literal into its one use never changes another
    // register's def/use count, so the counts are stable and computed once. The
    // previous version recomputed them and inlined one literal per pass, which was
    // O(n^2) and pathological on very large functions.
    let mut def_count: HashMap<u32, usize> = HashMap::new();
    let mut use_count: HashMap<u32, usize> = HashMap::new();
    for stmt in statements.iter() {
        if let Statement::Assign { target: AssignTarget::Register(r), .. } = stmt {
            *def_count.entry(*r).or_insert(0) += 1;
        }
        collect_value_reg_uses(stmt, &mut use_count);
    }

    // A register defined once as a pure object/array literal and used exactly once.
    // Restricted to composite literals so a bare constant / register copy is left
    // to the general inliner.
    let mut map: HashMap<u32, Expression> = HashMap::new();
    for stmt in statements.iter() {
        if let Statement::Assign { target: AssignTarget::Register(r), value } = stmt {
            let is_composite =
                matches!(value, Expression::Object { .. } | Expression::Array { .. });
            if is_composite
                && def_count.get(r) == Some(&1)
                && use_count.get(r) == Some(&1)
                && is_pure_literal(value)
            {
                map.insert(*r, value.clone());
            }
        }
    }
    if map.is_empty() {
        return;
    }

    // A literal may reference another eligible register (Hermes builds nested
    // objects across several registers). Resolve the map into itself so each value
    // embeds the others, to a fixed point bounded by the nesting depth.
    let keys: Vec<u32> = map.keys().copied().collect();
    for _ in 0..keys.len() {
        let mut changed = false;
        for &k in &keys {
            let mut v = map[&k].clone();
            substitute_registers_in_expr(&mut v, &map, Some(k));
            if v != map[&k] {
                map.insert(k, v);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // One substitution pass over the whole body, then drop the inlined defs.
    for stmt in statements.iter_mut() {
        substitute_registers_in_stmt(stmt, &map);
    }
    statements.retain(|stmt| {
        !matches!(stmt, Statement::Assign { target: AssignTarget::Register(r), .. } if map.contains_key(r))
    });
}

// Replace every `Register(r)` for which `map` has an entry with that entry's
// value, in one traversal. `exclude` skips a register (used while resolving the
// map into itself so a value is not substituted for its own register).
fn substitute_registers_in_stmt(stmt: &mut Statement, map: &std::collections::HashMap<u32, Expression>) {
    use crate::ir::MutVisitor;
    struct S<'a>(&'a std::collections::HashMap<u32, Expression>);
    impl<'a> MutVisitor for S<'a> {
        fn visit_expression(&mut self, e: &mut Expression) {
            if let Expression::Value(Value::Register(r)) = e {
                if let Some(v) = self.0.get(r) {
                    *e = v.clone();
                    return;
                }
            }
            self.walk_expression(e);
        }
    }
    S(map).visit_statement(stmt);
}

fn substitute_registers_in_expr(
    e: &mut Expression,
    map: &std::collections::HashMap<u32, Expression>,
    exclude: Option<u32>,
) {
    use crate::ir::MutVisitor;
    struct S<'a>(&'a std::collections::HashMap<u32, Expression>, Option<u32>);
    impl<'a> MutVisitor for S<'a> {
        fn visit_expression(&mut self, e: &mut Expression) {
            if let Expression::Value(Value::Register(r)) = e {
                if Some(*r) != self.1 {
                    if let Some(v) = self.0.get(r) {
                        *e = v.clone();
                        return;
                    }
                }
            }
            self.walk_expression(e);
        }
    }
    S(map, exclude).visit_expression(e);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Constant, VarKind};

    fn null() -> Expression {
        Expression::Value(Value::Constant(Constant::Null))
    }

    fn themes(prop: &str) -> Expression {
        Expression::Member {
            object: Box::new(Expression::Value(Value::Variable("Themes".into()))),
            property: PropertyKey::Ident(prop.into()),
            optional: false,
        }
    }

    fn index_assign(name: &str, slot: i32, value: Expression) -> Statement {
        Statement::Assign {
            target: AssignTarget::Index {
                object: Expression::Value(Value::Variable(name.into())),
                key: Expression::Value(Value::Constant(Constant::Integer(slot))),
            },
            value,
        }
    }

    #[test]
    fn folds_named_let_slot_fills() {
        let mut stmts = vec![
            Statement::Let {
                name: "obj".into(),
                value: Expression::Object {
                    properties: vec![
                        ObjectProperty {
                            key: PropertyKey::Ident("default".into()),
                            value: null(),
                        },
                        ObjectProperty {
                            key: PropertyKey::Ident("active".into()),
                            value: null(),
                        },
                    ],
                },
                kind: VarKind::Let,
            },
            index_assign("obj", 0, themes("DEFAULT")),
            index_assign("obj", 1, themes("ACTIVE")),
        ];
        fold_slot_index_fills(&mut stmts);
        assert_eq!(stmts.len(), 1, "fills should be consumed: {stmts:?}");
        match &stmts[0] {
            Statement::Let {
                value: Expression::Object { properties },
                ..
            } => {
                assert!(matches!(&properties[0].value, Expression::Member { .. }));
                assert!(matches!(&properties[1].value, Expression::Member { .. }));
            }
            other => panic!("expected folded Let object, got {other:?}"),
        }
    }

    #[test]
    fn does_not_fold_non_placeholder_numeric_key() {
        let mut stmts = vec![
            Statement::Let {
                name: "obj".into(),
                value: Expression::Object {
                    properties: vec![ObjectProperty {
                        key: PropertyKey::Ident("a".into()),
                        value: Expression::Value(Value::Constant(Constant::Integer(1))),
                    }],
                },
                kind: VarKind::Let,
            },
            index_assign("obj", 0, themes("DEFAULT")),
        ];
        fold_slot_index_fills(&mut stmts);
        assert_eq!(stmts.len(), 2, "non-placeholder must stay: {stmts:?}");
    }
}
