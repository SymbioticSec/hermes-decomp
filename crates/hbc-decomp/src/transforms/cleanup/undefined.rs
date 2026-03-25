use crate::ir::{is_undefined_expr, map_nested_bodies, Statement, AssignTarget};

pub(super) fn remove_undefined_initializations(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut result = Vec::new();
    let mut iter = stmts.into_iter().peekable();

    while let Some(stmt) = iter.next() {
        // Check if this is `r = undefined`
        if let Statement::Assign {
            target: AssignTarget::Register(r),
            value,
        } = &stmt
        {
            if is_undefined_expr(value) {
                if let Some(next) = iter.peek() {
                    if assigns_to_register(next, *r) {
                        continue;
                    }
                }
            }
        }

        result.push(map_nested_bodies(stmt, remove_undefined_initializations));
    }

    result
}

fn assigns_to_register(stmt: &Statement, reg: u32) -> bool {
    matches!(stmt, Statement::Assign { target: AssignTarget::Register(r), .. } if *r == reg)
}
