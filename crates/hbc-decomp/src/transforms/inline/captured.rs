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
            let mut r = Refs {
                reached: &mut refs,
                local: HashSet::new(),
            };
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

// The names a body reaches for before binding them itself. A name whose first
// occurrence in the body is a plain write (`obj = X`, `let obj = …`) is the
// body's own local: every function has an `obj`, and counting those made the
// parent keep its own `obj = { nick }` unfolded for a child that never read it.
// A read, or a read-modify-write (`count = count + 1`, visited value first),
// is a reach into the enclosing scope.
struct Refs<'a> {
    reached: &'a mut HashSet<String>,
    local: HashSet<String>,
}

impl<'b> Visitor<'b> for Refs<'_> {
    fn visit_statement(&mut self, s: &'b Statement) {
        match s {
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Variable(n)),
                value,
            } => {
                self.visit_expression(value);
                if !self.reached.contains(n) {
                    self.local.insert(n.clone());
                }
            }
            Statement::Let { name, value, .. } => {
                self.visit_expression(value);
                if !self.reached.contains(name) {
                    self.local.insert(name.clone());
                }
            }
            _ => self.walk_statement(s),
        }
    }
    fn visit_assign_target(&mut self, t: &'b AssignTarget) {
        if let AssignTarget::Binding(Binding::Variable(n)) = t {
            if !self.local.contains(n) {
                self.reached.insert(n.clone());
            }
        }
        self.walk_assign_target(t);
    }
    fn visit_expression(&mut self, e: &'b Expression) {
        if let Expression::Value(Value::Binding(Binding::Variable(n))) = e {
            if !self.local.contains(n) {
                self.reached.insert(n.clone());
            }
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

    #[test]
    fn a_childs_own_local_does_not_pin_the_parents_name() {
        let var = |n: &str| Expression::Value(Value::Binding(Binding::Variable(n.into())));
        let mut all_ir = BTreeMap::new();
        all_ir.insert(1, vec![]);
        all_ir.insert(
            2,
            vec![
                // `obj = X; obj.dispatch(); count = count + 1`
                Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable("obj".into())),
                    value: var("X"),
                },
                Statement::Expr(Expression::call(
                    Expression::member(var("obj"), "dispatch"),
                    vec![],
                )),
                Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable("count".into())),
                    value: Expression::Binary {
                        op: crate::ir::BinaryOp::Add,
                        left: Box::new(var("count")),
                        right: Box::new(Expression::constant(crate::ir::Constant::Integer(1))),
                    },
                },
            ],
        );
        let mut parent_of = BTreeMap::new();
        parent_of.insert(2, 1);
        let used = names_used_by_descendants(&all_ir, &parent_of);
        assert!(!used[&1].contains("obj"), "{:?}", used[&1]);
        assert!(used[&1].contains("count"));
        assert!(used[&1].contains("X"));
    }
}
