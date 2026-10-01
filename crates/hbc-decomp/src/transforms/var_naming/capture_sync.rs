// Give every capture the name its owner actually binds.
//
// A capture is baked into a name once, from the slot's value at that moment.
// The owner's own store for the same slot is baked separately, and later
// passes rename one side without the other: the owner of React's pooled-event
// helpers bound `releasePooledEvent`, the readers were baked `closure_1_40`
// and then named `release` from their own usage. This pass runs after every
// naming pass and renames a capture to the owner's current binding whenever
// the two differ. Nothing is invented: the owner's name comes from the
// bake record and must still be bound in the owner's body.

use std::collections::{BTreeMap, HashSet};

use crate::analysis::naming::rename_variables_in_stmts;
use crate::analysis::ClosureContext;
use crate::ir::{AssignTarget, Binding, Statement, Visitor};

pub fn sync_capture_names(
    all_ir: &mut BTreeMap<u32, Vec<Statement>>,
    closure_ctx: &ClosureContext,
) -> usize {
    let mut bound: BTreeMap<u32, HashSet<String>> = BTreeMap::new();
    let mut refs: BTreeMap<u32, HashSet<String>> = BTreeMap::new();
    for (&fid, stmts) in all_ir.iter() {
        let mut b = Bound(HashSet::new());
        let mut r = Refs(HashSet::new());
        for s in stmts {
            b.visit_statement(s);
            r.visit_statement(s);
        }
        bound.insert(fid, b.0);
        refs.insert(fid, r.0);
    }
    // owner -> slot -> the name the owner's store was baked as
    let mut owner_names: BTreeMap<u32, BTreeMap<u32, &String>> = BTreeMap::new();
    for (&fid, baked) in &closure_ctx.baked_captures {
        for (name, &(level, slot)) in baked {
            if level == 0 {
                owner_names.entry(fid).or_default().insert(slot, name);
            }
        }
    }
    let empty = HashSet::new();
    let mut total = 0;
    for (&fid, baked) in &closure_ctx.baked_captures {
        let mut renames: BTreeMap<String, String> = BTreeMap::new();
        let here_refs = refs.get(&fid).unwrap_or(&empty);
        let mut merged: Option<crate::analysis::ClosureInfo> = None;
        for (name, &(level, slot)) in baked {
            if level == 0 {
                continue;
            }
            let Some(owner) = closure_ctx.slot_owner(fid, level) else {
                continue;
            };
            let Some(owner_name) = owner_names.get(&owner).and_then(|m| m.get(&slot)) else {
                continue;
            };
            // The name this body uses for the slot now: the baked one if it
            // is still there, otherwise what a later pass (W10, inherit)
            // renamed it to, which is the context's current name for the
            // ancestor key.
            let current = if here_refs.contains(name.as_str()) {
                name.clone()
            } else {
                let info = merged.get_or_insert_with(|| closure_ctx.get_closure_info_for(fid));
                info.get_slot_name(crate::analysis::closure::info::encode_level_slot(
                    level, slot,
                ))
            };
            if *owner_name == &current || !here_refs.contains(current.as_str()) {
                continue;
            }
            let owner_bound = bound.get(&owner).unwrap_or(&empty);
            let here_bound = bound.get(&fid).unwrap_or(&empty);
            // The owner must still bind that name, and this body must not
            // bind it itself (a local of the same name would be shadowed).
            if !owner_bound.contains(owner_name.as_str())
                || here_bound.contains(owner_name.as_str())
            {
                continue;
            }
            renames.insert(current, (*owner_name).clone());
        }
        if renames.is_empty() {
            continue;
        }
        if let Some(stmts) = all_ir.get_mut(&fid) {
            total += renames.len();
            rename_variables_in_stmts(stmts, &renames);
        }
    }
    total
}

struct Refs(HashSet<String>);

impl<'a> Visitor<'a> for Refs {
    fn visit_assign_target(&mut self, t: &'a AssignTarget) {
        if let AssignTarget::Binding(Binding::Variable(n)) = t {
            self.0.insert(n.clone());
        }
        self.walk_assign_target(t);
    }
    fn visit_expression(&mut self, e: &'a crate::ir::Expression) {
        if let crate::ir::Expression::Value(crate::ir::Value::Binding(Binding::Variable(n))) = e {
            self.0.insert(n.clone());
        }
        self.walk_expression(e);
    }
}

struct Bound(HashSet<String>);

impl<'a> Visitor<'a> for Bound {
    fn visit_assign_target(&mut self, t: &'a AssignTarget) {
        if let AssignTarget::Binding(Binding::Variable(n)) = t {
            self.0.insert(n.clone());
        }
        self.walk_assign_target(t);
    }
    fn visit_binding_def(&mut self, name: &'a str) {
        self.0.insert(name.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Expression, Value};

    fn var(n: &str) -> Expression {
        Expression::Value(Value::Binding(Binding::Variable(n.into())))
    }

    #[test]
    fn a_capture_takes_the_owners_current_binding() {
        // owner 1 binds slot 40 as `releasePooledEvent`; child 2 read it as `release`
        let mut all_ir = BTreeMap::new();
        all_ir.insert(
            1,
            vec![Statement::Assign {
                target: AssignTarget::Binding(Binding::Variable("releasePooledEvent".into())),
                value: Expression::constant(crate::ir::Constant::Integer(1)),
            }],
        );
        all_ir.insert(2, vec![Statement::Expr(var("release"))]);
        let mut ctx = ClosureContext::new();
        ctx.add_child(1, 2);
        ctx.baked_captures
            .entry(1)
            .or_default()
            .insert("releasePooledEvent".into(), (0, 40));
        ctx.baked_captures
            .entry(2)
            .or_default()
            .insert("release".into(), (1, 40));
        assert_eq!(sync_capture_names(&mut all_ir, &ctx), 1);
        assert_eq!(all_ir[&2][0].to_string().trim(), "releasePooledEvent;");
    }
}
