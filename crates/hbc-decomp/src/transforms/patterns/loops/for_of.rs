use crate::ir::{Statement, Expression, Value, Constant, PropertyKey, AssignTarget};

// Detect for-of loop patterns.
pub fn detect_for_of_loops(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut result = Vec::new();
    let mut iter = stmts.into_iter().peekable();

    while let Some(stmt) = iter.next() {
        // Look for: iter = source[Symbol.iterator]()
        if let Statement::Assign { target: AssignTarget::Register(iter_reg), value } = &stmt {
            if let Some((source_expr, _)) = is_iterator_call(value) {
                // Check if next statement is a while(true) loop
                if let Some(Statement::While { condition, body }) = iter.peek() {
                    if is_true_condition(condition) {
                        if let Some((var_name, loop_body)) = extract_for_of_body(body, *iter_reg) {
                            // Found for-of pattern!
                            iter.next(); // consume the while
                            result.push(Statement::ForOf {
                                variable: var_name,
                                iterable: source_expr.clone(),
                                body: detect_for_of_loops(loop_body),
                            });
                            continue;
                        }
                    }
                }
            }
        }

        // Recursively transform nested statements
        let transformed = match stmt {
            Statement::While { condition, body } => Statement::While {
                condition,
                body: detect_for_of_loops(body),
            },
            Statement::If { condition, then_body, else_body } => Statement::If {
                condition,
                then_body: detect_for_of_loops(then_body),
                else_body: detect_for_of_loops(else_body),
            },
            Statement::Block(inner) => Statement::Block(detect_for_of_loops(inner)),
            Statement::For { init, condition, update, body } => Statement::For {
                init,
                condition,
                update,
                body: detect_for_of_loops(body),
            },
            other => other,
        };
        result.push(transformed);
    }

    result
}

// Check if expression is a call to [Symbol.iterator]()
fn is_iterator_call(expr: &Expression) -> Option<(Expression, ())> {
    if let Expression::Call { callee, arguments } = expr {
        if arguments.is_empty() {
            if let Expression::Member { object, property, .. } = callee.as_ref() {
                // Check for [Symbol.iterator] pattern
                if let PropertyKey::Computed(computed) = property {
                    if let Expression::Member { object: symbol_obj, property: PropertyKey::Ident(iter_prop), .. } = computed.as_ref() {
                        if let Expression::Value(Value::Variable(name)) = symbol_obj.as_ref() {
                            if name == "Symbol" && iter_prop == "iterator" {
                                return Some((*object.clone(), ()));
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

// Check if condition is `true`
fn is_true_condition(expr: &Expression) -> bool {
    matches!(expr, Expression::Value(Value::Constant(Constant::Bool(true))))
}

// Extract for-of loop body from while body.
fn extract_for_of_body(body: &[Statement], iter_reg: u32) -> Option<(String, Vec<Statement>)> {
    if body.len() < 3 {
        return None;
    }

    // First statement: result = iter.next()
    let result_reg = if let Statement::Assign { target: AssignTarget::Register(r), value } = &body[0] {
        if let Expression::Call { callee, arguments } = value {
            if arguments.is_empty() {
                if let Expression::Member { object, property: PropertyKey::Ident(prop), .. } = callee.as_ref() {
                    if prop == "next" {
                        if let Expression::Value(Value::Register(iter_r)) = object.as_ref() {
                            if *iter_r == iter_reg {
                                Some(*r)
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
        }
    } else {
        None
    }?;

    // Second statement: if (result.done) break  (or similar pattern)
    // This might be: if (result.done) { break } or if (!result.done) { ... } else { break }
    let body_start = if let Statement::If { condition, then_body, else_body: _ } = &body[1] {
        // Check if condition is result.done
        if is_done_check(condition, result_reg) {
            // if (result.done) break pattern
            if then_body.iter().any(|s| matches!(s, Statement::Comment(c) if c == "break")) ||
               then_body.is_empty() {
                2 // body starts at index 2
            } else {
                return None;
            }
        } else {
            return None;
        }
    } else {
        return None;
    };

    // Third statement: item = result.value
    let (item_name, value_stmt_idx) = if let Statement::Assign { target, value } = &body[body_start] {
        if let Expression::Member { object, property: PropertyKey::Ident(prop), .. } = value {
            if prop == "value" {
                if let Expression::Value(Value::Register(r)) = object.as_ref() {
                    if *r == result_reg {
                        let name = match target {
                            AssignTarget::Register(r) => format!("item{r}"),
                            AssignTarget::Variable(v) => v.clone(),
                            _ => return None,
                        };
                        Some((name, body_start + 1))
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

    // Remaining statements are the loop body
    let loop_body = body[value_stmt_idx..].to_vec();

    Some((item_name, loop_body))
}

// Check if expression is result.done
fn is_done_check(expr: &Expression, result_reg: u32) -> bool {
    if let Expression::Member { object, property: PropertyKey::Ident(prop), .. } = expr {
        if prop == "done" {
            if let Expression::Value(Value::Register(r)) = object.as_ref() {
                return *r == result_reg;
            }
        }
    }
    false
}
