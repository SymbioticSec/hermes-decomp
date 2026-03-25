// Arguments copy pattern simplification.
//
// Simplify Babel arguments-to-array copy pattern.
//
// Detects:
//   const length = arguments.length;
//   const array = new Array();
//   if (0 >= length) { BODY } else { array[i] = arguments[i]; while (i + 1 < length) {} }
//
// Replaces with:
//   const array = [...arguments];
//   BODY

use crate::ir::{map_nested_bodies_mut, AssignTarget, Expression, Statement, Value, VarKind};

// Simplify Babel arguments-to-array copy pattern.
pub fn simplify_arguments_copy(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut result: Vec<Statement> = Vec::with_capacity(stmts.len());
    let mut iter = stmts.into_iter().peekable();

    while let Some(stmt) = iter.next() {
        // Try to detect: length = arguments.length
        let length_var = match &stmt {
            Statement::Let { name, value, .. } => {
                if is_arguments_length(value) { Some(name.clone()) } else { None }
            }
            Statement::Assign { target: AssignTarget::Variable(name), value } => {
                if is_arguments_length(value) { Some(name.clone()) } else { None }
            }
            _ => None,
        };

        if let Some(length_name) = length_var {
            // Check next: array = new Array()
            if let Some(next) = iter.peek() {
                let array_info = match next {
                    Statement::Let { name, value, kind } => {
                        if is_empty_new_array(value) {
                            Some((name.clone(), true, Some(*kind)))
                        } else { None }
                    }
                    Statement::Assign { target: AssignTarget::Variable(name), value } => {
                        if is_empty_new_array(value) {
                            Some((name.clone(), false, None))
                        } else { None }
                    }
                    _ => None,
                };

                if let Some((array_name, is_let, var_kind)) = array_info {
                    iter.next(); // consume array declaration

                    // Check next: if (0 >= length) { BODY } else { COPY_LOOP }
                    if let Some(if_stmt) = iter.peek() {
                        let body_opt = extract_arguments_copy_body(if_stmt, &length_name, &array_name);
                        if let Some(body) = body_opt {
                            iter.next(); // consume if/else

                            // Emit: array = [...arguments]
                            let spread_args = Expression::Array {
                                elements: vec![Some(Expression::Spread(Box::new(
                                    Expression::Value(Value::Variable("arguments".to_string()))
                                )))],
                            };
                            if is_let {
                                result.push(Statement::Let {
                                    name: array_name,
                                    value: spread_args,
                                    kind: var_kind.unwrap_or(VarKind::Const),
                                });
                            } else {
                                result.push(Statement::Assign {
                                    target: AssignTarget::Variable(array_name),
                                    value: spread_args,
                                });
                            }

                            // Emit BODY statements
                            for mut s in body {
                                simplify_arguments_copy_recurse(&mut s);
                                result.push(s);
                            }
                            continue;
                        }
                    }

                    // Pattern didn't match fully -- emit both consumed statements
                    let mut s = stmt;
                    simplify_arguments_copy_recurse(&mut s);
                    result.push(s);

                    // Re-emit the array declaration we consumed
                    // (we already consumed it from iter, reconstruct it)
                    let arr_stmt = if is_let {
                        Statement::Let {
                            name: array_name,
                            value: Expression::New {
                                callee: Box::new(Expression::Value(Value::Variable("Array".to_string()))),
                                arguments: vec![],
                            },
                            kind: var_kind.unwrap_or(VarKind::Const),
                        }
                    } else {
                        Statement::Assign {
                            target: AssignTarget::Variable(array_name),
                            value: Expression::New {
                                callee: Box::new(Expression::Value(Value::Variable("Array".to_string()))),
                                arguments: vec![],
                            },
                        }
                    };
                    result.push(arr_stmt);
                    continue;
                }
            }

            // Only matched length, not array -- emit normally
            let mut s = stmt;
            simplify_arguments_copy_recurse(&mut s);
            result.push(s);
            continue;
        }

        let mut s = stmt;
        simplify_arguments_copy_recurse(&mut s);
        result.push(s);
    }

    result
}

// Check if expression is `arguments.length`
fn is_arguments_length(expr: &Expression) -> bool {
    if let Expression::Member { object, property: crate::ir::PropertyKey::Ident(prop), .. } = expr {
        if prop == "length" {
            return matches!(&**object,
                Expression::Value(Value::Variable(v)) if v == "arguments"
            ) || matches!(&**object, Expression::Value(Value::Arguments));
        }
    }
    false
}

// Check if expression is `new Array()` (empty) -- matches Variable("Array") and globalThis.Array
fn is_empty_new_array(expr: &Expression) -> bool {
    if let Expression::New { callee, arguments } = expr {
        if !arguments.is_empty() {
            return false;
        }
        return match &**callee {
            Expression::Value(Value::Variable(v)) => v == "Array",
            Expression::Member { object, property: crate::ir::PropertyKey::Ident(prop), .. } => {
                prop == "Array" && (
                    matches!(&**object, Expression::Value(Value::Global))
                    || matches!(&**object, Expression::Value(Value::Variable(v)) if v == "globalThis")
                )
            }
            _ => false,
        };
    }
    false
}

// Extract BODY from `if (!(0 < length)) { BODY } else { COPY_LOOP }` pattern.
// Also handles `if (0 >= length)` and `if (length <= 0)` forms.
// Returns the then-branch body if the pattern matches.
fn extract_arguments_copy_body(stmt: &Statement, length_name: &str, _array_name: &str) -> Option<Vec<Statement>> {
    if let Statement::If { condition, then_body, else_body } = stmt {
        // Check condition: 0 >= length (or length <= 0 or !(0 < length))
        let is_zero_ge_length = match condition {
            Expression::Binary { op: crate::ir::BinaryOp::Ge, left, right } => {
                is_zero_or_const(left) && is_var_named(right, length_name)
            }
            Expression::Binary { op: crate::ir::BinaryOp::Le, left, right } => {
                is_var_named(left, length_name) && is_zero_or_const(right)
            }
            // !(0 < length) -- structure recovery often emits this form
            Expression::Unary { op: crate::ir::UnaryOp::Not, operand } => {
                match &**operand {
                    Expression::Binary { op: crate::ir::BinaryOp::Lt, left, right } => {
                        is_zero_or_const(left) && is_var_named(right, length_name)
                    }
                    Expression::Binary { op: crate::ir::BinaryOp::Gt, left, right } => {
                        is_var_named(left, length_name) && is_zero_or_const(right)
                    }
                    _ => false,
                }
            }
            _ => false,
        };

        if !is_zero_ge_length {
            return None;
        }

        // Check else_body has the copy loop pattern (arguments reference + while loop)
        let has_copy_pattern = else_body.iter().any(|s| {
            matches!(s, Statement::While { .. })
        }) && else_body.iter().any(|s| {
            stmt_references_arguments(s)
        });

        if has_copy_pattern {
            return Some(then_body.clone());
        }
    }
    None
}

fn is_zero_or_const(expr: &Expression) -> bool {
    match expr {
        Expression::Value(Value::Constant(crate::ir::Constant::Integer(0))) => true,
        Expression::Value(Value::Constant(crate::ir::Constant::Number(n))) => *n == 0.0,
        _ => false,
    }
}

fn is_var_named(expr: &Expression, name: &str) -> bool {
    matches!(expr, Expression::Value(Value::Variable(v)) if v == name)
}

fn stmt_references_arguments(stmt: &Statement) -> bool {
    match stmt {
        Statement::Assign { value, target, .. } => {
            expr_references_arguments(value) || target_references_arguments(target)
        }
        Statement::Let { value, .. } => expr_references_arguments(value),
        Statement::Expr(e) => expr_references_arguments(e),
        _ => false,
    }
}

fn target_references_arguments(target: &AssignTarget) -> bool {
    match target {
        AssignTarget::Index { object, key } => {
            expr_references_arguments(object) || expr_references_arguments(key)
        }
        AssignTarget::Member { object, .. } => expr_references_arguments(object),
        _ => false,
    }
}

fn expr_references_arguments(expr: &Expression) -> bool {
    match expr {
        Expression::Value(Value::Arguments) => true,
        Expression::Value(Value::Variable(v)) if v == "arguments" => true,
        Expression::Member { object, .. } => expr_references_arguments(object),
        Expression::Binary { left, right, .. } => {
            expr_references_arguments(left) || expr_references_arguments(right)
        }
        Expression::Call { callee, arguments } => {
            expr_references_arguments(callee) || arguments.iter().any(expr_references_arguments)
        }
        Expression::Array { elements } => {
            elements.iter().flatten().any(expr_references_arguments)
        }
        _ => false,
    }
}

fn simplify_arguments_copy_recurse(stmt: &mut Statement) {
    map_nested_bodies_mut(stmt, simplify_arguments_copy);
}
