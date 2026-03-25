use crate::ir::{Expression, Statement, Value, Visitor};
use std::collections::BTreeMap;

pub fn is_chain_candidate(expr: &Expression) -> bool {
    matches!(expr, Expression::Member { .. })
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
