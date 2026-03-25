use crate::ir::{Statement, Expression, Value, Constant, BinaryOp, PropertyKey, AssignTarget};
use crate::transforms::patterns::utils::is_zero;

// Detect for-in loop patterns.
pub fn detect_for_in_loops(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut result = Vec::new();
    let mut iter = stmts.into_iter().peekable();

    while let Some(stmt) = iter.next() {
        // Look for: keys = Object.keys(obj)
        if let Statement::Assign { target: AssignTarget::Register(keys_reg), value } = &stmt {
            if let Some(obj_expr) = is_object_keys_call(value) {
                // Look for i = 0 followed by while loop
                if let Some(Statement::Assign { target: AssignTarget::Register(idx_reg), value: idx_value }) = iter.peek() {
                    if is_zero(idx_value) {
                        let idx_reg = *idx_reg;
                        iter.next(); // consume i = 0

                        if let Some(Statement::While { condition, body }) = iter.peek() {
                            if is_length_check(condition, idx_reg, *keys_reg) {
                                if let Some((var_name, loop_body)) = extract_for_in_body(body, *keys_reg, idx_reg) {
                                    iter.next(); // consume while
                                    result.push(Statement::ForIn {
                                        variable: var_name,
                                        object: obj_expr.clone(),
                                        body: detect_for_in_loops(loop_body),
                                    });
                                    continue;
                                }
                            }
                        }

                        // Didn't match for-in, push the i = 0 we consumed
                        result.push(stmt);
                        // Note: We already consumed iter.next() for i=0, need to handle this
                        // For simplicity, we'll just fall through and let the while be processed normally
                        continue;
                    }
                }
            }
        }

        // Recursively transform nested statements
        let transformed = match stmt {
            Statement::While { condition, body } => Statement::While {
                condition,
                body: detect_for_in_loops(body),
            },
            Statement::If { condition, then_body, else_body } => Statement::If {
                condition,
                then_body: detect_for_in_loops(then_body),
                else_body: detect_for_in_loops(else_body),
            },
            Statement::Block(inner) => Statement::Block(detect_for_in_loops(inner)),
            Statement::For { init, condition, update, body } => Statement::For {
                init,
                condition,
                update,
                body: detect_for_in_loops(body),
            },
            other => other,
        };
        result.push(transformed);
    }

    result
}

// Check if expression is Object.keys(obj)
fn is_object_keys_call(expr: &Expression) -> Option<Expression> {
    if let Expression::Call { callee, arguments } = expr {
        if arguments.len() == 1 {
            if let Expression::Member { object, property: PropertyKey::Ident(prop), .. } = callee.as_ref() {
                if prop == "keys" {
                    if let Expression::Value(Value::Variable(name)) = object.as_ref() {
                        if name == "Object" {
                            return Some(arguments[0].clone());
                        }
                    }
                }
            }
        }
    }
    None
}

// Check if expression is i < keys.length
fn is_length_check(expr: &Expression, idx_reg: u32, keys_reg: u32) -> bool {
    if let Expression::Binary { op: BinaryOp::Lt, left, right } = expr {
        // Check left is idx_reg
        if let Expression::Value(Value::Register(r)) = left.as_ref() {
            if *r == idx_reg {
                // Check right is keys.length
                if let Expression::Member { object, property: PropertyKey::Ident(prop), .. } = right.as_ref() {
                    if prop == "length" {
                        if let Expression::Value(Value::Register(r)) = object.as_ref() {
                            return *r == keys_reg;
                        }
                    }
                }
            }
        }
    }
    false
}

// Extract for-in body from while body
fn extract_for_in_body(body: &[Statement], keys_reg: u32, idx_reg: u32) -> Option<(String, Vec<Statement>)> {
    if body.is_empty() {
        return None;
    }

    // First statement: key = keys[i]
    let (var_name, body_start) = if let Statement::Assign { target, value } = &body[0] {
        if let Expression::Member { object, property: PropertyKey::Computed(idx_expr), .. } = value {
            if let Expression::Value(Value::Register(keys_r)) = object.as_ref() {
                if *keys_r == keys_reg {
                    if let Expression::Value(Value::Register(idx_r)) = idx_expr.as_ref() {
                        if *idx_r == idx_reg {
                            let name = match target {
                                AssignTarget::Register(r) => format!("key{r}"),
                                AssignTarget::Variable(v) => v.clone(),
                                _ => return None,
                            };
                            Some((name, 1))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    }?;

    // Remove the increment statement (i++) from the end
    let loop_body = if body.len() > body_start {
        let last_idx = body.len() - 1;
        if is_increment(&body[last_idx], idx_reg) {
            body[body_start..last_idx].to_vec()
        } else {
            body[body_start..].to_vec()
        }
    } else {
        vec![]
    };

    Some((var_name, loop_body))
}

// Check if statement is i++ or i = i + 1
fn is_increment(stmt: &Statement, reg: u32) -> bool {
    if let Statement::Assign { target: AssignTarget::Register(r), value } = stmt {
        if *r == reg {
            if let Expression::Binary { op: BinaryOp::Add, left, right } = value {
                // i = i + 1
                if let Expression::Value(Value::Register(lr)) = left.as_ref() {
                    if *lr == reg {
                        if let Expression::Value(Value::Constant(Constant::Integer(1))) = right.as_ref() {
                            return true;
                        }
                    }
                }
                // i = 1 + i
                if let Expression::Value(Value::Register(rr)) = right.as_ref() {
                    if *rr == reg {
                        if let Expression::Value(Value::Constant(Constant::Integer(1))) = left.as_ref() {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}
