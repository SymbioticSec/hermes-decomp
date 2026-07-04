use crate::ir::{BinaryOp, Constant, Expression, Statement, UnaryOp, Value};

// Convert a bottom-tested `while (true) { <body>; if (EXIT) { break; } }` — where
// the exit test is the LAST statement — into the faithful, far more readable
// `do { <body> } while (!EXIT)`.
//
// Hermes compiles `for`/`while` loops to a guarded do-while shape (the exit test
// sits at the loop latch, i.e. the bottom). Structure recovery emits that as
// `while (true)` plus an internal `if (EXIT) break;`. Recovering the do/while is
// closer to the original source and easier to audit.
//
// The rewrite is only applied when `<body>` contains no loop-level `continue` or
// `break` (those change meaning under do-while's bottom condition — a `continue`
// under `while (true)` skips the trailing exit test, but under `do/while` it runs
// the condition), so it is always semantics-preserving.
pub fn convert_while_true_loops(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts.into_iter().map(convert_stmt).collect()
}

fn convert_stmt(stmt: Statement) -> Statement {
    match stmt {
        Statement::While { condition, body } => {
            let body = convert_while_true_loops(body);
            if is_true(&condition) {
                if let Some((inner, exit)) = split_trailing_exit(&body) {
                    return Statement::DoWhile {
                        body: inner,
                        condition: negate(exit),
                    };
                }
            }
            Statement::While { condition, body }
        }
        Statement::DoWhile { body, condition } => Statement::DoWhile {
            body: convert_while_true_loops(body),
            condition,
        },
        Statement::For { init, condition, update, body } => Statement::For {
            init,
            condition,
            update,
            body: convert_while_true_loops(body),
        },
        Statement::ForIn { variable, object, body } => Statement::ForIn {
            variable,
            object,
            body: convert_while_true_loops(body),
        },
        Statement::ForOf { variable, iterable, body } => Statement::ForOf {
            variable,
            iterable,
            body: convert_while_true_loops(body),
        },
        Statement::If { condition, then_body, else_body } => Statement::If {
            condition,
            then_body: convert_while_true_loops(then_body),
            else_body: convert_while_true_loops(else_body),
        },
        Statement::Block(inner) => Statement::Block(convert_while_true_loops(inner)),
        Statement::TryCatch { try_body, catch_param, catch_body, finally_body } => Statement::TryCatch {
            try_body: convert_while_true_loops(try_body),
            catch_param,
            catch_body: convert_while_true_loops(catch_body),
            finally_body: convert_while_true_loops(finally_body),
        },
        Statement::Switch { discriminant, cases, default } => Statement::Switch {
            discriminant,
            cases: cases
                .into_iter()
                .map(|(e, b)| (e, convert_while_true_loops(b)))
                .collect(),
            default: default.map(convert_while_true_loops),
        },
        other => other,
    }
}

fn is_true(e: &Expression) -> bool {
    matches!(e, Expression::Value(Value::Constant(Constant::Bool(true))))
}

// If the body's last statement is `if (EXIT) { break; }` (empty else) and the
// rest of the body has no loop-level break/continue, return (body-minus-last, EXIT).
fn split_trailing_exit(body: &[Statement]) -> Option<(Vec<Statement>, Expression)> {
    let last = body.last()?;
    let Statement::If { condition, then_body, else_body } = last else {
        return None;
    };
    // then must be exactly `break;` (unlabeled → this loop).
    if !matches!(then_body.as_slice(), [Statement::Break(None)]) {
        return None;
    }
    // else must be absent or a redundant `continue;` (the implicit loop-back that
    // later cleanup would drop). Anything else means it is not a plain exit test.
    let else_ok = else_body.is_empty() || matches!(else_body.as_slice(), [Statement::Continue(None)]);
    if !else_ok {
        return None;
    }
    let inner = &body[..body.len() - 1];
    if inner.iter().any(|s| escapes_loop(s, false)) {
        return None;
    }
    Some((inner.to_vec(), condition.clone()))
}

// Does `stmt` contain a `break`/`continue` that targets the CURRENT loop
// (not a nested loop, and not a `break` that targets an enclosing `switch`)?
fn escapes_loop(stmt: &Statement, in_switch: bool) -> bool {
    match stmt {
        Statement::Continue(_) => true,      // always targets the enclosing loop
        Statement::Break(_) => !in_switch,   // `break` targets a switch if inside one
        Statement::If { then_body, else_body, .. } => then_body
            .iter()
            .chain(else_body)
            .any(|s| escapes_loop(s, in_switch)),
        Statement::Block(inner) => inner.iter().any(|s| escapes_loop(s, in_switch)),
        Statement::TryCatch { try_body, catch_body, finally_body, .. } => try_body
            .iter()
            .chain(catch_body)
            .chain(finally_body)
            .any(|s| escapes_loop(s, in_switch)),
        Statement::Switch { cases, default, .. } => cases
            .iter()
            .flat_map(|(_, b)| b)
            .chain(default.iter().flatten())
            .any(|s| escapes_loop(s, true)),
        // Nested loops swallow their own break/continue.
        Statement::While { .. }
        | Statement::DoWhile { .. }
        | Statement::For { .. }
        | Statement::ForIn { .. }
        | Statement::ForOf { .. } => false,
        _ => false,
    }
}

// Logical negation of a loop exit test, simplifying comparison operators so
// `!(i >= len)` renders as the clean `i < len` rather than `!(i >= len)`.
fn negate(e: Expression) -> Expression {
    match e {
        Expression::Binary { op, left, right } => {
            let inverted = match op {
                BinaryOp::Lt => Some(BinaryOp::Ge),
                BinaryOp::Le => Some(BinaryOp::Gt),
                BinaryOp::Gt => Some(BinaryOp::Le),
                BinaryOp::Ge => Some(BinaryOp::Lt),
                BinaryOp::Eq => Some(BinaryOp::Neq),
                BinaryOp::Neq => Some(BinaryOp::Eq),
                BinaryOp::StrictEq => Some(BinaryOp::StrictNeq),
                BinaryOp::StrictNeq => Some(BinaryOp::StrictEq),
                _ => None,
            };
            match inverted {
                Some(new_op) => Expression::Binary { op: new_op, left, right },
                None => Expression::unary(UnaryOp::Not, Expression::Binary { op, left, right }),
            }
        }
        // `!!x` -> `x`
        Expression::Unary { op: UnaryOp::Not, operand } => *operand,
        other => Expression::unary(UnaryOp::Not, other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(n: &str) -> Expression {
        Expression::Value(Value::Variable(n.to_string()))
    }
    fn cmp(op: BinaryOp, l: &str, r: &str) -> Expression {
        Expression::Binary { op, left: Box::new(var(l)), right: Box::new(var(r)) }
    }
    fn if_break(cond: Expression, else_body: Vec<Statement>) -> Statement {
        Statement::If { condition: cond, then_body: vec![Statement::Break(None)], else_body }
    }
    fn while_true(body: Vec<Statement>) -> Statement {
        Statement::While { condition: Expression::constant(Constant::Bool(true)), body }
    }

    #[test]
    fn converts_trailing_break_to_dowhile() {
        // while (true) { i = i + 1; if (i >= n) break; }  ->  do { i = i + 1 } while (i < n)
        let inc = Statement::Expr(var("work"));
        let input = vec![while_true(vec![inc.clone(), if_break(cmp(BinaryOp::Ge, "i", "n"), vec![])])];
        let out = convert_while_true_loops(input);
        match &out[0] {
            Statement::DoWhile { body, condition } => {
                assert_eq!(body.len(), 1); // the `if break` was consumed
                // `i >= n` negated to `i < n`
                assert!(matches!(condition, Expression::Binary { op: BinaryOp::Lt, .. }));
            }
            other => panic!("expected DoWhile, got {other:?}"),
        }
    }

    #[test]
    fn accepts_redundant_continue_else() {
        // else branch is a redundant `continue;` -> still converts
        let input = vec![while_true(vec![
            Statement::Expr(var("work")),
            if_break(cmp(BinaryOp::Ge, "i", "n"), vec![Statement::Continue(None)]),
        ])];
        let out = convert_while_true_loops(input);
        assert!(matches!(out[0], Statement::DoWhile { .. }));
    }

    #[test]
    fn does_not_convert_with_loop_level_continue() {
        // A `continue` in the body changes meaning under do/while -> must NOT convert.
        let input = vec![while_true(vec![
            Statement::If {
                condition: var("skip"),
                then_body: vec![Statement::Continue(None)],
                else_body: vec![],
            },
            Statement::Expr(var("work")),
            if_break(cmp(BinaryOp::Ge, "i", "n"), vec![]),
        ])];
        let out = convert_while_true_loops(input);
        assert!(matches!(out[0], Statement::While { .. }), "must stay while(true) when body has continue");
    }

    #[test]
    fn break_in_nested_switch_is_ok() {
        // `break` inside a switch targets the switch, not the loop -> conversion allowed.
        let sw = Statement::Switch {
            discriminant: var("x"),
            cases: vec![(var("a"), vec![Statement::Break(None)])],
            default: None,
        };
        let input = vec![while_true(vec![sw, if_break(cmp(BinaryOp::Ge, "i", "n"), vec![])])];
        let out = convert_while_true_loops(input);
        assert!(matches!(out[0], Statement::DoWhile { .. }));
    }
}
