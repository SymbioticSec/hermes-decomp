use crate::ir::{AssignTarget, BinaryOp, Constant, Expression, Statement, Value};

// Detect and simplify generator state machine patterns.
//
// Transpiled Generators (Regenerator) often use a "State Machine Loop":
// ```js
// var state = 0;
// while (true) {
//   switch (state) {
//     case 0: ...; state = 2; break;
//     case 2: ...; return;
//   }
// }
// ```
// This pass tries to detect this pattern (While -> Switch -> Case assignments) and "flatten" it
// into a linear sequence of statements, removing the artificial state variable.
pub fn simplify_state_machine(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut result = Vec::new();
    let mut i = 0;

    while i < stmts.len() {
        let stmt = &stmts[i];

        // Look for state machine pattern:
        // 1. state = 0 (or state initialization)
        // 2. while (true) { switch (state) { ... } }
        // or just: switch (state) { case 0: ... case 1: ... }

        if let Some(flattened) = try_flatten_state_machine(stmt) {
            result.extend(flattened);
        } else {
            result.push(simplify_state_machine_stmt(stmts[i].clone()));
        }

        i += 1;
    }

    result
}

// Try to flatten a state machine switch into sequential code.
fn try_flatten_state_machine(stmt: &Statement) -> Option<Vec<Statement>> {
    // Look for infinite loop with switch
    if let Statement::While { condition, body } = stmt {
        if is_true_literal(condition) && body.len() == 1 {
            if let Some(Statement::If { .. }) = body.first() {
                // This might be a state machine - try to flatten
                return flatten_state_switch(body);
            }
        }
    }

    // Direct switch-like if-else chain
    if let Statement::If { .. } = stmt {
        if looks_like_state_machine(stmt) {
            return flatten_state_if_chain(stmt);
        }
    }

    None
}

// Check if a condition is `true` literal.
fn is_true_literal(expr: &Expression) -> bool {
    matches!(
        expr,
        Expression::Value(Value::Constant(Constant::Bool(true)))
    )
}

// Check if an if-else chain looks like a state machine.
fn looks_like_state_machine(stmt: &Statement) -> bool {
    // Count depth of if-else chain and check for state-like comparisons
    let mut depth = 0;
    let mut current = stmt;

    while let Statement::If {
        condition,
        else_body,
        ..
    } = current
    {
        // Check if condition is comparing a register to a constant
        if let Expression::Binary {
            op: BinaryOp::StrictEq,
            left,
            right,
        } = condition
        {
            if matches!(left.as_ref(), Expression::Value(Value::Register(_)))
                && matches!(
                    right.as_ref(),
                    Expression::Value(Value::Constant(Constant::Integer(_)))
                )
            {
                depth += 1;
            }
        }

        if else_body.len() == 1 {
            current = &else_body[0];
        } else {
            break;
        }
    }

    depth >= 2 // At least 2 states to be considered a state machine
}

// Flatten a state switch into sequential statements.
fn flatten_state_switch(body: &[Statement]) -> Option<Vec<Statement>> {
    // Collect case bodies in order
    let mut cases: Vec<(i32, Vec<Statement>)> = Vec::new();

    collect_state_cases(&body[0], &mut cases);

    if cases.is_empty() {
        return None;
    }

    // Sort by state number
    cases.sort_by_key(|(state, _)| *state);

    // Flatten: emit each case's body (filtering out state assignments and breaks)
    let mut result = Vec::new();
    for (_, case_body) in cases {
        for stmt in case_body {
            // Skip state assignments (state = N)
            if is_state_assignment_ref(&stmt) {
                continue;
            }
            // Skip break statements
            if matches!(&stmt, Statement::Comment(c) if c == "break") {
                continue;
            }
            result.push(stmt);
        }
    }

    Some(result)
}

// Collect state cases from an if-else chain.
fn collect_state_cases(stmt: &Statement, cases: &mut Vec<(i32, Vec<Statement>)>) {
    if let Statement::If {
        condition,
        then_body,
        else_body,
    } = stmt
    {
        // Extract state number from condition
        if let Expression::Binary {
            op: BinaryOp::StrictEq,
            left,
            right,
        } = condition
        {
            if let Expression::Value(Value::Constant(Constant::Integer(state))) = right.as_ref() {
                cases.push((*state, then_body.clone()));
            } else if let Expression::Value(Value::Constant(Constant::Integer(state))) =
                left.as_ref()
            {
                cases.push((*state, then_body.clone()));
            }
        }

        // Recurse into else branch
        if else_body.len() == 1 {
            collect_state_cases(&else_body[0], cases);
        } else if !else_body.is_empty() {
            // This is the default case
            cases.push((i32::MAX, else_body.clone()));
        }
    }
}

// Flatten an if-else chain that looks like a state machine.
fn flatten_state_if_chain(stmt: &Statement) -> Option<Vec<Statement>> {
    flatten_state_switch(&[stmt.clone()])
}

// Check if a statement is a state assignment (state = N).
fn is_state_assignment_ref(stmt: &Statement) -> bool {
    matches!(
        stmt,
        Statement::Assign {
            target: AssignTarget::Register(_),
            value: Expression::Value(Value::Constant(Constant::Integer(_)))
        }
    )
}

// Recursively simplify state machine patterns in a statement.
fn simplify_state_machine_stmt(stmt: Statement) -> Statement {
    match stmt {
        Statement::If {
            condition,
            then_body,
            else_body,
        } => Statement::If {
            condition,
            then_body: simplify_state_machine(then_body),
            else_body: simplify_state_machine(else_body),
        },
        Statement::While { condition, body } => {
            // Try to detect and simplify state machine in the while body
            let simplified_body = simplify_state_machine(body);
            Statement::While {
                condition,
                body: simplified_body,
            }
        }
        Statement::For {
            init,
            condition,
            update,
            body,
        } => Statement::For {
            init: init.map(|s| Box::new(simplify_state_machine_stmt(*s))),
            condition,
            update: update.map(|s| Box::new(simplify_state_machine_stmt(*s))),
            body: simplify_state_machine(body),
        },
        Statement::TryCatch {
            try_body,
            catch_param,
            catch_body,
            finally_body,
        } => Statement::TryCatch {
            try_body: simplify_state_machine(try_body),
            catch_param,
            catch_body: simplify_state_machine(catch_body),
            finally_body: simplify_state_machine(finally_body),
        },
        Statement::Block(inner) => Statement::Block(simplify_state_machine(inner)),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_machine_detection() {
        // Simulate a state machine if-else chain
        let stmt = Statement::If {
            condition: Expression::Binary {
                op: BinaryOp::StrictEq,
                left: Box::new(Expression::Value(Value::Register(0))),
                right: Box::new(Expression::Value(Value::Constant(Constant::Integer(0)))),
            },
            then_body: vec![Statement::Return(Some(Expression::Value(Value::Constant(
                Constant::Integer(1),
            ))))],
            else_body: vec![Statement::If {
                condition: Expression::Binary {
                    op: BinaryOp::StrictEq,
                    left: Box::new(Expression::Value(Value::Register(0))),
                    right: Box::new(Expression::Value(Value::Constant(Constant::Integer(1)))),
                },
                then_body: vec![Statement::Return(Some(Expression::Value(Value::Constant(
                    Constant::Integer(2),
                ))))],
                else_body: vec![Statement::Return(Some(Expression::Value(Value::Constant(
                    Constant::Integer(3),
                ))))],
            }],
        };

        assert!(looks_like_state_machine(&stmt));
    }
}
