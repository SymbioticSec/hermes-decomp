use crate::ir::{AssignTarget, Expression, Statement, Value, VarKind};

// Fold the guarded do-while shape Hermes emits for `for`/`while` loops back into a
// natural `for`/`while`.
//
// Hermes lowers loops with "loop inversion": the test is emitted twice, once as a
// top guard and once at the bottom (the back-edge). After `convert_while_true_loops`
// that surfaces as:
//
//     INIT(i = e);
//     if (GUARD) { do { BODY; UPDATE(i = …) } while (COND) }   // for-loop
//     if (COND)  { do { BODY } while (COND) }                   // while-loop
//
// where GUARD is COND evaluated with the initial value (`test[i := e]`). We recover:
//
//     for (i = e; COND; UPDATE) { BODY }
//     while (COND) { BODY }
//
// This is only applied when it is provably equivalent: GUARD must structurally equal
// COND with the loop variable substituted by its init value, and (for the `for` case)
// the loop variable must not be read after the loop, so moving its declaration into
// the `for` header cannot change observable scope. The do-while produced upstream is
// already guaranteed free of loop-level `continue`, so `continue` semantics are moot.
pub fn fold_guarded_loops(stmts: Vec<Statement>) -> Vec<Statement> {
    let recursed: Vec<Statement> = stmts.into_iter().map(fold_in_stmt).collect();
    fold_sequence(recursed)
}

fn fold_in_stmt(stmt: Statement) -> Statement {
    match stmt {
        Statement::While { condition, body } => Statement::While { condition, body: fold_guarded_loops(body) },
        Statement::DoWhile { body, condition } => Statement::DoWhile { body: fold_guarded_loops(body), condition },
        Statement::For { init, condition, update, body } => Statement::For { init, condition, update, body: fold_guarded_loops(body) },
        Statement::ForIn { variable, object, body } => Statement::ForIn { variable, object, body: fold_guarded_loops(body) },
        Statement::ForOf { variable, iterable, body } => Statement::ForOf { variable, iterable, body: fold_guarded_loops(body) },
        Statement::If { condition, then_body, else_body } => Statement::If {
            condition,
            then_body: fold_guarded_loops(then_body),
            else_body: fold_guarded_loops(else_body),
        },
        Statement::Block(inner) => Statement::Block(fold_guarded_loops(inner)),
        Statement::TryCatch { try_body, catch_param, catch_body, finally_body } => Statement::TryCatch {
            try_body: fold_guarded_loops(try_body),
            catch_param,
            catch_body: fold_guarded_loops(catch_body),
            finally_body: fold_guarded_loops(finally_body),
        },
        Statement::Switch { discriminant, cases, default } => Statement::Switch {
            discriminant,
            cases: cases.into_iter().map(|(e, b)| (e, fold_guarded_loops(b))).collect(),
            default: default.map(fold_guarded_loops),
        },
        other => other,
    }
}

fn fold_sequence(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut out: Vec<Statement> = Vec::with_capacity(stmts.len());
    let mut i = 0;
    while i < stmts.len() {
        // for-loop: INIT immediately followed by `if (GUARD) { do { … } while (COND) }`.
        if i + 1 < stmts.len() {
            if let Some((var, init_val)) = loop_var_init(&stmts[i]) {
                let rest = &stmts[i + 2..];
                if let Some(for_stmt) = try_fold_for(&stmts[i], &var, &init_val, &stmts[i + 1], rest) {
                    out.push(for_stmt);
                    i += 2;
                    continue;
                }
            }
        }
        // while-loop: `if (T) { do { … } while (T) }` with an identical guard/condition.
        if let Some(while_stmt) = try_fold_while(&stmts[i]) {
            out.push(while_stmt);
            i += 1;
            continue;
        }
        out.push(stmts[i].clone());
        i += 1;
    }
    out
}

// The loop variable name + its initial value, from `let v = e;` or `v = e;`.
fn loop_var_init(stmt: &Statement) -> Option<(String, Expression)> {
    match stmt {
        Statement::Let { name, value, .. } => Some((name.clone(), value.clone())),
        Statement::Assign { target: AssignTarget::Variable(name), value } => {
            Some((name.clone(), value.clone()))
        }
        _ => None,
    }
}

fn try_fold_for(
    init_stmt: &Statement,
    var: &str,
    init_val: &Expression,
    guard_if: &Statement,
    rest: &[Statement],
) -> Option<Statement> {
    let (guard, dowhile) = as_guarded_dowhile(guard_if)?;
    let Statement::DoWhile { body, condition: cond } = dowhile else {
        return None;
    };

    // Body must end with an update to the loop variable (`v = v <op> …`).
    let last = body.last()?;
    let Statement::Assign { target: AssignTarget::Variable(upd_name), value: upd_val } = last else {
        return None;
    };
    if upd_name != var || !expr_mentions_var(upd_val, var) {
        return None;
    }
    // The condition must actually test the loop variable.
    if !expr_mentions_var(cond, var) {
        return None;
    }
    // GUARD must be COND evaluated at the initial value, proves the top test is the
    // loop's entry test, so `if (GUARD) do…while(COND)` == `for(init; COND; update)`.
    if substitute_var(cond.clone(), var, init_val) != *guard {
        return None;
    }
    // Moving `let v = …` into the `for` header narrows its scope: only safe if the
    // loop variable is never read after the loop.
    if rest.iter().any(|s| stmt_mentions_var(s, var)) {
        return None;
    }

    // Emit the init as a `let` declaration so the loop variable is scoped to the
    // `for` header (a bare `num = 0` would leak an implicit global under strict
    // mode). Safe because we already required the variable is unused after the loop.
    let for_init = match init_stmt {
        Statement::Let { .. } => init_stmt.clone(),
        _ => Statement::Let {
            name: var.to_string(),
            value: init_val.clone(),
            kind: VarKind::Let,
        },
    };

    let update = last.clone();
    let new_body: Vec<Statement> = body[..body.len() - 1].to_vec();
    Some(Statement::For {
        init: Some(Box::new(for_init)),
        condition: Some(cond.clone()),
        update: Some(Box::new(update)),
        body: new_body,
    })
}

// `if (T) { do { BODY } while (T) }` with structurally identical guard/condition
// (and no trailing update to fold into a `for`) becomes `while (T) { BODY }`.
fn try_fold_while(stmt: &Statement) -> Option<Statement> {
    let (guard, dowhile) = as_guarded_dowhile(stmt)?;
    let Statement::DoWhile { body, condition: cond } = dowhile else {
        return None;
    };
    if cond != guard {
        return None;
    }
    Some(Statement::While {
        condition: cond.clone(),
        body: body.clone(),
    })
}

// Match `if (GUARD) { <single do-while> }` with an empty else, returning (GUARD, do-while).
fn as_guarded_dowhile(stmt: &Statement) -> Option<(&Expression, &Statement)> {
    let Statement::If { condition, then_body, else_body } = stmt else {
        return None;
    };
    if !else_body.is_empty() {
        return None;
    }
    // Ignore leftover label comments (`// label0:`) from loop reconstruction.
    let mut body = then_body.iter().filter(|s| !matches!(s, Statement::Comment(_)));
    let first = body.next()?;
    if body.next().is_some() {
        return None; // more than just the do-while
    }
    match first {
        Statement::DoWhile { body: dw_body, .. } if !has_labeled_jump(dw_body) => Some((condition, first)),
        _ => None,
    }
}

// A labeled `break`/`continue` (`break label0;`) would be orphaned if we drop the
// loop's label by folding into a plain `for`/`while` (neither carries a label).
fn has_labeled_jump(stmts: &[Statement]) -> bool {
    stmts.iter().any(|s| match s {
        Statement::Break(Some(_)) | Statement::Continue(Some(_)) => true,
        Statement::If { then_body, else_body, .. } => {
            has_labeled_jump(then_body) || has_labeled_jump(else_body)
        }
        Statement::While { body, .. }
        | Statement::DoWhile { body, .. }
        | Statement::For { body, .. }
        | Statement::ForIn { body, .. }
        | Statement::ForOf { body, .. }
        | Statement::Block(body) => has_labeled_jump(body),
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
            has_labeled_jump(try_body) || has_labeled_jump(catch_body) || has_labeled_jump(finally_body)
        }
        Statement::Switch { cases, default, .. } => {
            cases.iter().any(|(_, b)| has_labeled_jump(b))
                || default.as_deref().is_some_and(has_labeled_jump)
        }
        _ => false,
    })
}

fn substitute_var(expr: Expression, var: &str, repl: &Expression) -> Expression {
    match expr {
        Expression::Value(Value::Variable(ref n)) if n == var => repl.clone(),
        Expression::Binary { op, left, right } => Expression::Binary {
            op,
            left: Box::new(substitute_var(*left, var, repl)),
            right: Box::new(substitute_var(*right, var, repl)),
        },
        Expression::Unary { op, operand } => Expression::Unary {
            op,
            operand: Box::new(substitute_var(*operand, var, repl)),
        },
        Expression::Member { object, property, optional } => Expression::Member {
            object: Box::new(substitute_var(*object, var, repl)),
            property,
            optional,
        },
        other => other,
    }
}

fn expr_mentions_var(expr: &Expression, var: &str) -> bool {
    match expr {
        Expression::Value(Value::Variable(n)) => n == var,
        Expression::Binary { left, right, .. } => {
            expr_mentions_var(left, var) || expr_mentions_var(right, var)
        }
        Expression::Unary { operand, .. } => expr_mentions_var(operand, var),
        Expression::Member { object, .. } => expr_mentions_var(object, var),
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            expr_mentions_var(callee, var) || arguments.iter().any(|a| expr_mentions_var(a, var))
        }
        Expression::Conditional { condition, then_expr, else_expr } => {
            expr_mentions_var(condition, var)
                || expr_mentions_var(then_expr, var)
                || expr_mentions_var(else_expr, var)
        }
        Expression::Assignment { target, value } => {
            expr_mentions_var(target, var) || expr_mentions_var(value, var)
        }
        Expression::Array { elements } => elements.iter().flatten().any(|e| expr_mentions_var(e, var)),
        Expression::Object { properties } => properties.iter().any(|p| expr_mentions_var(&p.value, var)),
        Expression::Spread(inner) => expr_mentions_var(inner, var),
        Expression::TemplateLiteral { expressions, .. } => {
            expressions.iter().any(|e| expr_mentions_var(e, var))
        }
        _ => false,
    }
}

// Conservative: does the statement (recursively) mention the variable anywhere?
fn stmt_mentions_var(stmt: &Statement, var: &str) -> bool {
    let e = |ex: &Expression| expr_mentions_var(ex, var);
    let b = |body: &[Statement]| body.iter().any(|s| stmt_mentions_var(s, var));
    match stmt {
        Statement::Expr(x) | Statement::Return(Some(x)) | Statement::Throw(x) => e(x),
        Statement::Let { value, .. } => e(value),
        Statement::Assign { target, value } => assign_target_mentions(target, var) || e(value),
        Statement::If { condition, then_body, else_body } => e(condition) || b(then_body) || b(else_body),
        Statement::While { condition, body } | Statement::DoWhile { body, condition } => e(condition) || b(body),
        Statement::For { init, condition, update, body } => {
            init.as_deref().is_some_and(|s| stmt_mentions_var(s, var))
                || condition.as_ref().is_some_and(e)
                || update.as_deref().is_some_and(|s| stmt_mentions_var(s, var))
                || b(body)
        }
        Statement::ForIn { object: x, body, .. } | Statement::ForOf { iterable: x, body, .. } => e(x) || b(body),
        Statement::Block(inner) => b(inner),
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => b(try_body) || b(catch_body) || b(finally_body),
        Statement::Switch { discriminant, cases, default } => {
            e(discriminant)
                || cases.iter().any(|(v, body)| e(v) || b(body))
                || default.as_deref().is_some_and(b)
        }
        _ => false,
    }
}

fn assign_target_mentions(t: &AssignTarget, var: &str) -> bool {
    match t {
        AssignTarget::Variable(n) => n == var,
        AssignTarget::Member { object, .. } => expr_mentions_var(object, var),
        AssignTarget::Index { object, key } => expr_mentions_var(object, var) || expr_mentions_var(key, var),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{BinaryOp, Constant};

    fn var(n: &str) -> Expression {
        Expression::Value(Value::Variable(n.to_string()))
    }
    fn lt(l: Expression, r: Expression) -> Expression {
        Expression::Binary { op: BinaryOp::Lt, left: Box::new(l), right: Box::new(r) }
    }
    fn int(v: i32) -> Expression {
        Expression::constant(Constant::Integer(v))
    }
    fn let_(name: &str, v: Expression) -> Statement {
        Statement::Let { name: name.to_string(), value: v, kind: VarKind::Let }
    }
    fn assign(name: &str, v: Expression) -> Statement {
        Statement::Assign { target: AssignTarget::Variable(name.to_string()), value: v }
    }
    fn inc(name: &str) -> Statement {
        assign(name, Expression::Binary { op: BinaryOp::Add, left: Box::new(var(name)), right: Box::new(int(1)) })
    }
    fn guarded(guard: Expression, dw_body: Vec<Statement>, cond: Expression) -> Statement {
        Statement::If {
            condition: guard,
            then_body: vec![Statement::DoWhile { body: dw_body, condition: cond }],
            else_body: vec![],
        }
    }

    #[test]
    fn folds_guarded_dowhile_to_for() {
        // let i = 0; if (0 < n) do { work; i = i + 1 } while (i < n)
        //   -> for (let i = 0; i < n; i = i + 1) { work }
        let input = vec![
            let_("i", int(0)),
            guarded(lt(int(0), var("n")), vec![Statement::Expr(var("work")), inc("i")], lt(var("i"), var("n"))),
        ];
        let out = fold_guarded_loops(input);
        assert_eq!(out.len(), 1);
        match &out[0] {
            Statement::For { init, condition, update, body } => {
                assert!(matches!(init.as_deref(), Some(Statement::Let { .. })));
                assert!(matches!(condition, Some(Expression::Binary { op: BinaryOp::Lt, .. })));
                assert!(matches!(update.as_deref(), Some(Statement::Assign { .. })));
                assert_eq!(body.len(), 1); // the increment was pulled into the header
            }
            other => panic!("expected For, got {other:?}"),
        }
    }

    #[test]
    fn does_not_fold_when_guard_mismatches_condition() {
        // guard `1 < n` is NOT `(i < n)[i:=0]` == `0 < n` -> keep do-while.
        let input = vec![
            let_("i", int(0)),
            guarded(lt(int(1), var("n")), vec![Statement::Expr(var("work")), inc("i")], lt(var("i"), var("n"))),
        ];
        let out = fold_guarded_loops(input);
        assert_eq!(out.len(), 2);
        assert!(matches!(out[1], Statement::If { .. }));
    }

    #[test]
    fn does_not_fold_for_when_var_used_after_loop() {
        // `i` is read after the loop -> moving its declaration into `for` would
        // change scope, so it must not fold to a `for`.
        let input = vec![
            let_("i", int(0)),
            guarded(lt(int(0), var("n")), vec![Statement::Expr(var("work")), inc("i")], lt(var("i"), var("n"))),
            Statement::Expr(var("i")),
        ];
        let out = fold_guarded_loops(input);
        assert!(matches!(out[0], Statement::Let { .. }));
        assert!(matches!(out[1], Statement::If { .. }));
    }

    #[test]
    fn folds_guarded_dowhile_to_while_when_no_update() {
        // if (T) do { work } while (T)  ->  while (T) { work }
        let t = lt(var("i"), var("n"));
        let input = vec![guarded(t.clone(), vec![Statement::Expr(var("work"))], t)];
        let out = fold_guarded_loops(input);
        assert!(matches!(out[0], Statement::While { .. }));
    }

    #[test]
    fn does_not_fold_with_labeled_jump() {
        // A labeled break inside would be orphaned by dropping the loop label.
        let input = vec![
            let_("i", int(0)),
            guarded(
                lt(int(0), var("n")),
                vec![Statement::Break(Some("outer".to_string())), inc("i")],
                lt(var("i"), var("n")),
            ),
        ];
        let out = fold_guarded_loops(input);
        assert_eq!(out.len(), 2);
        assert!(matches!(out[1], Statement::If { .. }));
    }
}
