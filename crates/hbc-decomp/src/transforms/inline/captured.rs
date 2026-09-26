// The variable names each function's nested functions read or write.
//
// After closure resolution a captured slot is a plain name in every body that
// touches it, so a pass looking at one body alone sees a store nobody reads
// and drops it. The map answers, for a function, which names its descendants
// still need bound.

use std::collections::{BTreeMap, HashSet};

use crate::ir::{AssignTarget, Binding, Expression, Statement, Value, Visitor};

pub fn names_used_by_descendants(
    all_ir: &BTreeMap<u32, Vec<Statement>>,
    parent_of: &BTreeMap<u32, u32>,
) -> BTreeMap<u32, HashSet<String>> {
    let mut out: BTreeMap<u32, HashSet<String>> = BTreeMap::new();
    for (&fid, stmts) in all_ir {
        let mut refs: HashSet<String> = HashSet::new();
        {
            let mut r = Refs(&mut refs);
            for s in stmts {
                r.visit_statement(s);
            }
        }
        if refs.is_empty() {
            continue;
        }
        let mut cur = fid;
        let mut hops = 0;
        while let Some(&parent) = parent_of.get(&cur) {
            if parent == cur || hops > 64 {
                break;
            }
            out.entry(parent).or_default().extend(refs.iter().cloned());
            cur = parent;
            hops += 1;
        }
    }
    out
}

struct Refs<'a>(&'a mut HashSet<String>);

impl<'b> Visitor<'b> for Refs<'_> {
    fn visit_assign_target(&mut self, t: &'b AssignTarget) {
        if let AssignTarget::Binding(Binding::Variable(n)) = t {
            self.0.insert(n.clone());
        }
        self.walk_assign_target(t);
    }
    fn visit_expression(&mut self, e: &'b Expression) {
        if let Expression::Value(Value::Binding(Binding::Variable(n))) = e {
            self.0.insert(n.clone());
        }
        self.walk_expression(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_read_by_a_grandchild_reaches_both_ancestors() {
        let mut all_ir = BTreeMap::new();
        all_ir.insert(1, vec![]);
        all_ir.insert(2, vec![]);
        all_ir.insert(
            3,
            vec![Statement::Expr(Expression::Value(Value::Binding(
                Binding::Variable("set".into()),
            )))],
        );
        let mut parent_of = BTreeMap::new();
        parent_of.insert(2, 1);
        parent_of.insert(3, 2);
        let used = names_used_by_descendants(&all_ir, &parent_of);
        assert!(used[&1].contains("set"));
        assert!(used[&2].contains("set"));
        assert!(!used.contains_key(&3));
    }
}
