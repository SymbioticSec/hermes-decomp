use crate::ir::{AssignTarget, BinaryOp, Expression, Statement, Value};

// Detect and transform default parameter patterns:
// `if (argN === undefined) argN = value`
// to `argN = argN ?? value`
pub fn transform_default_params(stmts: &mut [Statement]) {
    let mut i = 0;
    while i < stmts.len() {
        let mut replacement = None;
        if let Statement::If {
            condition,
            then_body,
            else_body,
        } = &stmts[i]
        {
            // Check if structure is `if (arg === undefined) { arg = val }`
            if else_body.is_empty() && then_body.len() == 1 {
                if let Some((param_reg, value)) = match_default_pattern(condition, &then_body[0]) {
                    // Transform to `arg = arg ?? value`
                    let target = AssignTarget::Register(param_reg);
                    let expr = Expression::binary(
                        BinaryOp::NullishCoalesce,
                        Expression::Value(Value::Register(param_reg)),
                        value.clone(),
                    );
                    replacement = Some(Statement::Assign {
                        target,
                        value: expr,
                    });
                }
            }
        }

        if let Some(stmt) = replacement {
            stmts[i] = stmt;
        }
        i += 1;
    }
}

fn match_default_pattern<'a>(
    condition: &'a Expression,
    body: &'a Statement,
) -> Option<(u32, &'a Expression)> {
    // Check condition: `argN === undefined` or `undefined === argN`
    // And body: `argN = value`

    // Extract param from condition
    let param = match condition {
        Expression::Binary {
            op: BinaryOp::StrictEq | BinaryOp::Eq,
            left,
            right,
        } => {
            if is_undefined(right) || is_null(right) {
                get_reg(left)
            } else if is_undefined(left) || is_null(left) {
                get_reg(right)
            } else {
                None
            }
        }
        _ => None,
    }?;

    // Check body assignment
    if let Statement::Assign {
        target: AssignTarget::Register(dst),
        value,
    } = body
    {
        if *dst == param {
            return Some((param, value));
        }
    }

    None
}

use super::patterns::utils::{is_null, is_undefined};

fn get_reg(expr: &Expression) -> Option<u32> {
    if let Expression::Value(Value::Register(r)) = expr {
        Some(*r)
    } else {
        None
    }
}
