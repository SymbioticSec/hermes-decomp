mod inlining;
mod usage;

use crate::ir::{map_nested_bodies, AssignTarget, Binding, Expression, Statement};
use inlining::inline_chains_in_stmt;
use std::collections::BTreeMap;
use std::collections::HashSet;
use usage::{count_register_defs, count_register_uses, is_chain_candidate};

pub fn optimize_chain_access(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut use_count: BTreeMap<u32, usize> = BTreeMap::new();
    let mut def_map: BTreeMap<u32, (usize, Expression)> = BTreeMap::new();
    // Count ALL register definitions, INCLUDING those nested inside branch/loop
    // bodies. A register defined once at the top level but reassigned inside an
    // `if` (e.g. a merge-carried value: `r = a; if (c) { r = b }; use(r)`) is
    // multi-def; inlining its top-level definition would drop the conditional
    // reassignment. `count_register_uses` already recurses, so the def count must
    // too, otherwise a flat count reports 1 def and the value is wrongly inlined.
    let mut def_count: BTreeMap<u32, usize> = BTreeMap::new();

    for (idx, stmt) in stmts.iter().enumerate() {
        count_register_uses(stmt, &mut use_count);
        count_register_defs(stmt, &mut def_count);

        if let Statement::Assign {
            target: AssignTarget::Binding(Binding::Register(r)),
            value,
        } = stmt
        {
            if is_chain_candidate(value) {
                def_map.insert(*r, (idx, value.clone()));
            }
        }
    }

    let mut to_inline: BTreeMap<u32, Expression> = BTreeMap::new();

    for (reg, (idx, expr)) in &def_map {
        if use_count.get(reg).copied().unwrap_or(0) == 1
            && def_count.get(reg).copied().unwrap_or(0) == 1
            && use_follows_without_interference(&stmts, *idx, *reg, expr)
        {
            to_inline.insert(*reg, expr.clone());
        }
    }

    let mut result: Vec<Statement> = Vec::with_capacity(stmts.len());
    let mut to_remove: HashSet<usize> = HashSet::new();

    for (reg, (idx, _)) in &def_map {
        if to_inline.contains_key(reg) {
            to_remove.insert(*idx);
        }
    }

    for (idx, stmt) in stmts.into_iter().enumerate() {
        if to_remove.contains(&idx) {
            continue;
        }

        let new_stmt = inline_chains_in_stmt(stmt, &to_inline);
        result.push(new_stmt);
    }

    result.into_iter().map(process_nested_chains).collect()
}

// A property read moves to its use only if nothing between the two can
// change what it reads: no call, no store, no control flow, and no write to a
// register the read depends on. The use itself has to be at the top level or
// in a condition evaluated first, never inside a nested body that other
// statements precede. `a[k] = v` read before a `try` that stores into `a[k]`
// was being carried past the store, and came out as `a[k] = a[k]`.
fn use_follows_without_interference(
    stmts: &[Statement],
    def_idx: usize,
    reg: u32,
    expr: &Expression,
) -> bool {
    use crate::ir::{expr_uses_register, stmt_has_side_effects, stmt_uses_register};
    for stmt in &stmts[def_idx + 1..] {
        if stmt_uses_register(stmt, reg) {
            return match stmt {
                Statement::Assign { .. }
                | Statement::Let { .. }
                | Statement::Expr(_)
                | Statement::Return(_)
                | Statement::Throw(_) => true,
                Statement::If { condition, .. } | Statement::While { condition, .. } => {
                    expr_uses_register(condition, reg)
                }
                Statement::Switch { discriminant, .. } => expr_uses_register(discriminant, reg),
                _ => false,
            };
        }
        if stmt_has_side_effects(stmt) {
            return false;
        }
        if let Statement::Assign {
            target: AssignTarget::Binding(Binding::Register(w)),
            ..
        } = stmt
        {
            if expr_uses_register(expr, *w) {
                return false;
            }
        }
    }
    false
}

fn process_nested_chains(stmt: Statement) -> Statement {
    map_nested_bodies(stmt, optimize_chain_access)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{AssignTarget, Expression, PropertyKey, Statement, Value};

    #[test]
    fn test_chain_access_inline() {
        // r0 = obj.a; r1 = r0.b; return r1;
        let obj = Expression::Value(Value::Binding(Binding::Variable("obj".to_string())));
        let stmts = vec![
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Register(0)),
                value: Expression::Member {
                    object: Box::new(obj),
                    property: PropertyKey::Ident("a".to_string()),
                    optional: false,
                },
            },
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Register(1)),
                value: Expression::Member {
                    object: Box::new(Expression::Value(Value::Binding(Binding::Register(0)))),
                    property: PropertyKey::Ident("b".to_string()),
                    optional: false,
                },
            },
            Statement::Return(Some(Expression::Value(Value::Binding(Binding::Register(
                1,
            ))))),
        ];

        let result = optimize_chain_access(stmts);

        // Should be: return obj.a.b;
        assert_eq!(result.len(), 1);
        if let Statement::Return(Some(Expression::Member {
            object,
            property: PropertyKey::Ident(prop),
            ..
        })) = &result[0]
        {
            assert_eq!(prop, "b");
            if let Expression::Member {
                object: inner,
                property: PropertyKey::Ident(inner_prop),
                ..
            } = object.as_ref()
            {
                assert_eq!(inner_prop, "a");
                assert!(
                    matches!(inner.as_ref(), Expression::Value(Value::Binding(Binding::Variable(v))) if v == "obj")
                );
            } else {
                panic!("Expected nested member access");
            }
        } else {
            panic!("Expected return with member chain, got: {:?}", result[0]);
        }
    }

    #[test]
    fn test_multi_use_not_inlined() {
        // r0 = obj.a; r1 = r0.b; r2 = r0.c; (r0 used twice)
        let obj = Expression::Value(Value::Binding(Binding::Variable("obj".to_string())));
        let stmts = vec![
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Register(0)),
                value: Expression::Member {
                    object: Box::new(obj),
                    property: PropertyKey::Ident("a".to_string()),
                    optional: false,
                },
            },
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Register(1)),
                value: Expression::Member {
                    object: Box::new(Expression::Value(Value::Binding(Binding::Register(0)))),
                    property: PropertyKey::Ident("b".to_string()),
                    optional: false,
                },
            },
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Register(2)),
                value: Expression::Member {
                    object: Box::new(Expression::Value(Value::Binding(Binding::Register(0)))),
                    property: PropertyKey::Ident("c".to_string()),
                    optional: false,
                },
            },
        ];

        let result = optimize_chain_access(stmts);

        // r0 should NOT be inlined because it's used twice
        // But r1 and r2 definitions should remain
        assert_eq!(result.len(), 3);
    }
}
