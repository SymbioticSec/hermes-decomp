use crate::ir::{AssignTarget, Expression, Statement, Value};

// Inline registers defined once as a pure object/array literal and used exactly
// once, regardless of statement order. Repeats to a fixed point so deep nests
// collapse fully.
pub(super) fn inline_single_use_literals(statements: &mut Vec<Statement>) {
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
