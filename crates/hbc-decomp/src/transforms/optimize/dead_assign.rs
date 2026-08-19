use crate::ir::{stmt_uses_register, AssignTarget, Expression, Statement, Value};

pub(super) fn remove_dead_assignments(stmts: Vec<Statement>) -> Vec<Statement> {
    // A store `X = value` is dead when, scanning forward in this flat sequence, a
    // later statement overwrites X before anything reads it. The scan stops at the
    // first control-flow statement (if/loop/return/...), which could read X in a
    // branch, so only straight-line runs are considered (safe and covers the Metro
    // bootstrap's flat `__d(...)` list). A dead pure store is dropped entirely; a
    // dead store of a side-effecting call keeps the call and drops the target.
    let n = stmts.len();
    let mut dead: Vec<bool> = vec![false; n];
    for (i, stmt) in stmts.iter().enumerate() {
        if let Statement::Assign { target, .. } = stmt {
            if let Some(key) = target_key(target) {
                if overwritten_before_read(&stmts, i + 1, &key) {
                    dead[i] = true;
                }
            }
        }
    }

    let mut result = Vec::with_capacity(n);
    for (i, stmt) in stmts.into_iter().enumerate() {
        if dead[i] {
            let Statement::Assign { value, .. } = stmt else { unreachable!() };
            if value.has_side_effects() {
                result.push(Statement::Expr(value));
            }
            // Pure dead store: drop entirely.
            continue;
        }
        let optimized = match stmt {
            Statement::If { condition, then_body, else_body } => Statement::If {
                condition,
                then_body: remove_dead_assignments(then_body),
                else_body: remove_dead_assignments(else_body),
            },
            Statement::While { condition, body } => Statement::While {
                condition,
                body: remove_dead_assignments(body),
            },
            Statement::Block(inner) => Statement::Block(remove_dead_assignments(inner)),
            _ => stmt,
        };
        result.push(optimized);
    }

    result
}

// Scan forward from `start` in a flat statement run: return true if `key` is
// overwritten by a full store before any statement reads it. Stops (returning
// false, i.e. treat as live) at the first control-flow statement, since a branch
// could read the value.
fn overwritten_before_read(stmts: &[Statement], start: usize, key: &Key) -> bool {
    for stmt in &stmts[start..] {
        if !is_straight_line(stmt) {
            return false;
        }
        if uses_key(stmt, key) {
            return false;
        }
        if overwrites(stmt, key) {
            return true;
        }
    }
    false
}

// A statement with no control flow of its own: a plain assignment or a bare
// expression. Anything else may read the value inside a branch/loop/return, so the
// forward scan stops there.
fn is_straight_line(stmt: &Statement) -> bool {
    matches!(stmt, Statement::Assign { .. } | Statement::Expr(_) | Statement::Let { .. })
}

// A location that a store can target and a later store can overwrite. Only simple
// register/variable targets are tracked; member/index stores are not dead-store
// candidates (they may alias).
enum Key {
    Reg(u32),
    Var(String),
}

fn target_key(target: &AssignTarget) -> Option<Key> {
    match target {
        AssignTarget::Register(r) => Some(Key::Reg(*r)),
        AssignTarget::Variable(name) => Some(Key::Var(name.clone())),
        _ => None,
    }
}

fn overwrites(stmt: &Statement, key: &Key) -> bool {
    match (stmt, key) {
        (Statement::Assign { target: AssignTarget::Register(r), .. }, Key::Reg(k)) => r == k,
        (Statement::Assign { target: AssignTarget::Variable(n), .. }, Key::Var(k)) => n == k,
        (Statement::Let { name, .. }, Key::Var(k)) => name == k,
        _ => false,
    }
}

fn uses_key(stmt: &Statement, key: &Key) -> bool {
    match key {
        Key::Reg(r) => stmt_uses_register(stmt, *r),
        Key::Var(name) => stmt_uses_variable(stmt, name),
    }
}

// Whether a statement reads variable `name` anywhere in its expressions.
fn stmt_uses_variable(stmt: &Statement, name: &str) -> bool {
    use crate::ir::Visitor;
    struct V<'a> {
        name: &'a str,
        found: bool,
    }
    impl<'a> Visitor<'a> for V<'_> {
        fn visit_expression(&mut self, e: &'a Expression) {
            if let Expression::Value(Value::Variable(n)) = e {
                if n == self.name {
                    self.found = true;
                }
            }
            self.walk_expression(e);
        }
    }
    let mut v = V { name, found: false };
    v.visit_statement(stmt);
    v.found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Constant, PropertyKey};

    fn call(name: &str) -> Expression {
        // A call has side effects by default in `has_side_effects`.
        Expression::Call {
            callee: Box::new(Expression::Value(Value::Variable(name.into()))),
            arguments: vec![Expression::Value(Value::Constant(Constant::Integer(0)))],
        }
    }

    #[test]
    fn dead_store_of_side_effecting_call_keeps_call_drops_target() {
        // `x = __d(0); x = __r(0);` -> the first store is dead, keep the call only.
        let stmts = vec![
            Statement::Assign { target: AssignTarget::Variable("x".into()), value: call("__d") },
            Statement::Assign { target: AssignTarget::Variable("x".into()), value: call("__r") },
        ];
        let out = remove_dead_assignments(stmts);
        // First becomes a bare Expr(call), second stays (last store, not overwritten).
        assert!(matches!(&out[0], Statement::Expr(Expression::Call { .. })));
        assert!(matches!(&out[1], Statement::Assign { .. }));
    }

    #[test]
    fn live_store_is_kept() {
        // `x = f(); g(x);` -> x is read, keep the assignment.
        let stmts = vec![
            Statement::Assign { target: AssignTarget::Variable("x".into()), value: call("f") },
            Statement::Expr(Expression::Call {
                callee: Box::new(Expression::Value(Value::Variable("g".into()))),
                arguments: vec![Expression::Value(Value::Variable("x".into()))],
            }),
        ];
        let out = remove_dead_assignments(stmts);
        assert!(matches!(&out[0], Statement::Assign { .. }));
    }

    #[test]
    fn read_before_overwrite_is_kept() {
        // `x = f(); y.k = x; x = g();` -> next stmt reads x, so the first store lives.
        let stmts = vec![
            Statement::Assign { target: AssignTarget::Variable("x".into()), value: call("f") },
            Statement::Assign {
                target: AssignTarget::Member {
                    object: Expression::Value(Value::Variable("y".into())),
                    property: "k".into(),
                },
                value: Expression::Value(Value::Variable("x".into())),
            },
            Statement::Assign { target: AssignTarget::Variable("x".into()), value: call("g") },
        ];
        let out = remove_dead_assignments(stmts);
        assert!(matches!(&out[0], Statement::Assign { .. }));
        let _ = PropertyKey::Ident("k".into());
    }
}
