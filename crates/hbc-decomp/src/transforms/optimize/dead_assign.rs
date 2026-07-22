use crate::ir::{stmt_uses_register, Statement, AssignTarget};

pub(super) fn remove_dead_assignments(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut result = Vec::with_capacity(stmts.len());
    let mut iter = stmts.into_iter().peekable();

    while let Some(stmt) = iter.next() {
        if let Statement::Assign { target: AssignTarget::Register(r), value } = &stmt {
            if let Some(next) = iter.peek() {
                // Drop `r = v; r = w` (first def dead) only when `v` is pure, a
                // call/await/etc. must still run for its side effect even if its
                // result register is immediately overwritten.
                if overwrites_register(next, *r)
                    && !stmt_uses_register(next, *r)
                    && !value.has_side_effects()
                {
                    continue;
                }
            }
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

fn overwrites_register(stmt: &Statement, reg: u32) -> bool {
    match stmt {
        Statement::Assign { target: AssignTarget::Register(r), .. } => *r == reg,
        _ => false,
    }
}
