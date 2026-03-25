use crate::ir::{AssignTarget, BinaryOp, Constant, Expression, Statement, Value};

pub fn transform_logic(stmts: &mut [Statement]) {
    for stmt in stmts.iter_mut() {
        if let Statement::If {
            condition,
            then_body,
            else_body,
        } = stmt
        {
            // Pattern: if (x) r = x; else r = y;  =>  r = x || y;
            // Pattern: if (x) r = y; else r = x;  =>  r = x && y;

            if then_body.len() == 1 && else_body.len() == 1 {
                if let (
                    Statement::Assign {
                        target: t1,
                        value: v1,
                    },
                    Statement::Assign {
                        target: t2,
                        value: v2,
                    },
                ) = (&then_body[0], &else_body[0])
                {
                    if t1 == t2 {
                        // Targets must match
                        // Case 1: r = x || y
                        // If condition == v1, then result is v1 (truthy), else v2.
                        // This matches `x || y` (if x is true, result is x, else y).
                        if are_equivalent(condition, v1) {
                            *stmt = Statement::Assign {
                                target: t1.clone(),
                                value: Expression::binary(
                                    BinaryOp::Or,
                                    condition.clone(),
                                    v2.clone(),
                                ),
                            };
                            continue;
                        }

                        // Case 2: r = x && y
                        // If condition (x) is true, result is v1 (y). Else result is v2 (x).
                        // This matches `x && y` (if x is true, result is y, else x).
                        if are_equivalent(condition, v2) {
                            *stmt = Statement::Assign {
                                target: t1.clone(),
                                value: Expression::binary(
                                    BinaryOp::And,
                                    condition.clone(),
                                    v1.clone(),
                                ),
                            };
                            continue;
                        }
                    }
                }
            }
        }
    }

    // Pass 2: Optional Chaining
    // Pattern: var t = x; if (t != null) t = t.prop;
    // In IR often: t = x; if (t) t = t.prop;
    let mut i = 0;
    while i + 1 < stmts.len() {
        let stmt1 = &stmts[i];
        let stmt2 = &stmts[i + 1];

        let mut replacement = None;
        if let Statement::Assign {
            target: t1,
            value: _v1,
        } = stmt1
        {
            if let AssignTarget::Register(r1) = t1 {
                if let Statement::If {
                    condition,
                    then_body,
                    else_body,
                } = stmt2
                {
                    if else_body.is_empty() && then_body.len() == 1 {
                        if let Statement::Assign {
                            target: t2,
                            value: v2,
                        } = &then_body[0]
                        {
                            if t2 == t1 {
                                // Check condition: if (r1) or if (r1 != null)
                                let is_null_check = match condition {
                                    Expression::Value(Value::Register(r)) => *r == *r1,
                                    Expression::Binary {
                                        op: BinaryOp::Neq | BinaryOp::StrictNeq,
                                        left,
                                        right,
                                    } => {
                                        // check r1 != null/undefined
                                        let checks_reg = is_reg(left, *r1) || is_reg(right, *r1);
                                        let checks_null = is_null_or_undefined(left)
                                            || is_null_or_undefined(right);
                                        checks_reg && checks_null
                                    }
                                    _ => false,
                                };

                                if is_null_check {
                                    // Check value: r1.prop
                                    if let Expression::Member {
                                        object, property, ..
                                    } = v2
                                    {
                                        if is_reg(object, *r1) {
                                            // Found optional chaining: t = t?.prop
                                            // But we have t = x; if (t) t = t.prop
                                            // We can combine into t = x?.prop
                                            // OR if t is reused, maybe just transform the IF?
                                            // Transform the IF into: t = t?.prop
                                            let new_expr = Expression::Member {
                                                object: Box::new(Expression::Value(
                                                    Value::Register(*r1),
                                                )),
                                                property: property.clone(),
                                                optional: true,
                                            };

                                            replacement = Some(Statement::Assign {
                                                target: t1.clone(),
                                                value: new_expr,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some(repl) = replacement {
            stmts[i + 1] = repl;
            // Optionally remove i if v1 is temp?
            // For now keep t = x; t = t?.prop; -> propagates later
        }
        i += 1;
    }
}

fn are_equivalent(e1: &Expression, e2: &Expression) -> bool {
    // Use PartialEq derivation for structural equality
    e1 == e2
}

fn is_reg(expr: &Expression, reg: u32) -> bool {
    matches!(expr, Expression::Value(Value::Register(r)) if *r == reg)
}

fn is_null_or_undefined(expr: &Expression) -> bool {
    if let Expression::Value(Value::Constant(c)) = expr {
        matches!(c, Constant::Null | Constant::Undefined)
    } else {
        false
    }
}
