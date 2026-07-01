use crate::ir::{AssignTarget, Expression, Statement, Value, Visitor};
use std::collections::BTreeMap;

pub fn is_chain_candidate(expr: &Expression) -> bool {
    matches!(expr, Expression::Member { .. })
}

// Count register definitions (assignment targets) across a statement AND all its
// nested bodies (if/while/for/...). A register defined at the top level but
// reassigned inside a branch is multi-def and must not be inlined from its
// top-level definition — counting only top-level defs would miss the nested one.
pub fn count_register_defs(stmt: &Statement, counts: &mut BTreeMap<u32, usize>) {
    let mut counter = DefCounter { counts };
    counter.visit_statement(stmt);
}

struct DefCounter<'c> {
    counts: &'c mut BTreeMap<u32, usize>,
}

impl<'a, 'c> Visitor<'a> for DefCounter<'c> {
    fn visit_assign_target(&mut self, target: &'a AssignTarget) {
        if let AssignTarget::Register(r) = target {
            *self.counts.entry(*r).or_insert(0) += 1;
        }
        self.walk_assign_target(target);
    }
    fn visit_expression(&mut self, expr: &'a Expression) {
        // A compound write target (e.g. inside Expression::Assignment) is a def.
        if let Expression::Assignment { target, .. } = expr {
            if let Expression::Value(Value::Register(r)) = &**target {
                *self.counts.entry(*r).or_insert(0) += 1;
            }
        }
        self.walk_expression(expr);
    }
}

// Count register uses in a single statement using the Visitor trait.
pub fn count_register_uses(stmt: &Statement, counts: &mut BTreeMap<u32, usize>) {
    let mut counter = RegisterCounter { counts };
    counter.visit_statement(stmt);
}

struct RegisterCounter<'c> {
    counts: &'c mut BTreeMap<u32, usize>,
}

impl<'a, 'c> Visitor<'a> for RegisterCounter<'c> {
    fn visit_expression(&mut self, expr: &'a Expression) {
        if let Expression::Value(Value::Register(r)) = expr {
            *self.counts.entry(*r).or_insert(0) += 1;
        }
        self.walk_expression(expr);
    }
}
