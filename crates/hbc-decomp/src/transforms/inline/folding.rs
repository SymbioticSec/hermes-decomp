// Object/Array literal folding passes.

use crate::ir::{map_nested_bodies_mut, AssignTarget, Expression, ObjectProperty, PropertyKey, Statement, Value, VarKind};

// === Object literal folding pass ===
// Folds `obj = {}; obj.a = 1; obj.b = 2;` -> `obj = { a: 1, b: 2 }`

// Fold sequential property assignments into the preceding object literal initialization.
// Works on both `let obj = {}; obj.a = 1;` and `obj = {}; obj.a = 1;` patterns.
pub fn fold_object_literals(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut result: Vec<Statement> = Vec::with_capacity(stmts.len());
    let mut iter = stmts.into_iter().peekable();

    while let Some(stmt) = iter.next() {
        // Try to detect an object initialization statement
        let (obj_name, mut properties, is_let, var_kind) = match &stmt {
            Statement::Let { name, value: Expression::Object { properties }, kind } => {
                (name.clone(), properties.clone(), true, Some(*kind))
            }
            Statement::Assign { target: AssignTarget::Variable(name), value: Expression::Object { properties } } => {
                (name.clone(), properties.clone(), false, None)
            }
            _ => {
                let mut s = stmt;
                fold_object_literals_recurse(&mut s);
                result.push(s);
                continue;
            }
        };

        // Collect consecutive property assignments to this object
        let mut folded_count = 0;
        while let Some(next) = iter.peek() {
            match next {
                Statement::Assign {
                    target: AssignTarget::Member { object, property },
                    value,
                } => {
                    // Check object is Variable(obj_name)
                    if !matches!(object, Expression::Value(Value::Variable(n)) if n == &obj_name) {
                        break;
                    }
                    // Don't fold if value references the object itself (e.g. obj.constructor = obj)
                    if expr_references_var(value, &obj_name) {
                        break;
                    }
                    // Check if property already exists -- if so, replace its value
                    let key = PropertyKey::Ident(property.clone());
                    if let Some(existing) = properties.iter_mut().find(|p| p.key == key) {
                        existing.value = value.clone();
                    } else {
                        properties.push(ObjectProperty {
                            key,
                            value: value.clone(),
                        });
                    }
                    folded_count += 1;
                    iter.next(); // consume
                }
                // Stop at any non-property-assignment statement
                _ => break,
            }
        }

        if folded_count > 0 {
            // Reconstruct with the folded properties
            let obj_expr = Expression::Object { properties };
            let mut s = if is_let {
                Statement::Let { name: obj_name, value: obj_expr, kind: var_kind.unwrap_or(VarKind::Let) }
            } else {
                Statement::Assign { target: AssignTarget::Variable(obj_name), value: obj_expr }
            };
            fold_object_literals_recurse(&mut s);
            result.push(s);
        } else {
            let mut s = stmt;
            fold_object_literals_recurse(&mut s);
            result.push(s);
        }
    }

    result
}

fn fold_object_literals_recurse(stmt: &mut Statement) {
    map_nested_bodies_mut(stmt, fold_object_literals);
}

// Check if an expression references a specific variable name.
fn expr_references_var(expr: &Expression, var_name: &str) -> bool {
    match expr {
        Expression::Value(Value::Variable(name)) => name == var_name,
        Expression::Binary { left, right, .. } => {
            expr_references_var(left, var_name) || expr_references_var(right, var_name)
        }
        Expression::Unary { operand, .. } => expr_references_var(operand, var_name),
        Expression::Call { callee, arguments } => {
            expr_references_var(callee, var_name)
                || arguments.iter().any(|a| expr_references_var(a, var_name))
        }
        Expression::New { callee, arguments } => {
            expr_references_var(callee, var_name)
                || arguments.iter().any(|a| expr_references_var(a, var_name))
        }
        Expression::Member { object, .. } => expr_references_var(object, var_name),
        Expression::Conditional { condition, then_expr, else_expr } => {
            expr_references_var(condition, var_name)
                || expr_references_var(then_expr, var_name)
                || expr_references_var(else_expr, var_name)
        }
        Expression::Array { elements } => {
            elements.iter().flatten().any(|e| expr_references_var(e, var_name))
        }
        Expression::Object { properties } => {
            properties.iter().any(|p| expr_references_var(&p.value, var_name))
        }
        Expression::Assignment { target, value } => {
            expr_references_var(target, var_name) || expr_references_var(value, var_name)
        }
        Expression::Spread(inner) => expr_references_var(inner, var_name),
        _ => false,
    }
}

// === Array literal folding pass ===
// Folds `items = []; items[0] = x; items[1] = y;` -> `items = [x, y]`
// Also handles sparse arrays: `items = [, ]; items[0] = x; items[1] = y;` -> `items = [x, y]`

// Fold sequential index assignments into the preceding array literal initialization.
pub fn fold_array_literals(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut result: Vec<Statement> = Vec::with_capacity(stmts.len());
    let mut iter = stmts.into_iter().peekable();

    while let Some(stmt) = iter.next() {
        // Try to detect an array initialization statement
        let (arr_name, mut elements, is_let, var_kind) = match &stmt {
            Statement::Let { name, value: Expression::Array { elements }, kind } => {
                (name.clone(), elements.clone(), true, Some(*kind))
            }
            Statement::Assign { target: AssignTarget::Variable(name), value: Expression::Array { elements } } => {
                (name.clone(), elements.clone(), false, None)
            }
            _ => {
                let mut s = stmt;
                fold_array_literals_recurse(&mut s);
                result.push(s);
                continue;
            }
        };

        // Collect consecutive index assignments to this array
        let mut folded_count = 0;
        while let Some(next) = iter.peek() {
            match next {
                Statement::Assign {
                    target: AssignTarget::Index { object, key },
                    value,
                } => {
                    // Check object is Variable(arr_name)
                    if !matches!(object, Expression::Value(Value::Variable(n)) if n == &arr_name) {
                        break;
                    }
                    // Check key is a constant integer index
                    let idx = match key {
                        Expression::Value(Value::Constant(crate::ir::Constant::Integer(i))) => *i as usize,
                        _ => break,
                    };
                    // Don't fold if value references the array itself
                    if expr_references_var(value, &arr_name) {
                        break;
                    }
                    // Extend elements array if needed
                    while elements.len() <= idx {
                        elements.push(None);
                    }
                    elements[idx] = Some(value.clone());
                    folded_count += 1;
                    iter.next(); // consume
                }
                _ => break,
            }
        }

        if folded_count > 0 {
            let arr_expr = Expression::Array { elements };
            let mut s = if is_let {
                Statement::Let { name: arr_name, value: arr_expr, kind: var_kind.unwrap_or(VarKind::Let) }
            } else {
                Statement::Assign { target: AssignTarget::Variable(arr_name), value: arr_expr }
            };
            fold_array_literals_recurse(&mut s);
            result.push(s);
        } else {
            let mut s = stmt;
            fold_array_literals_recurse(&mut s);
            result.push(s);
        }
    }

    result
}

fn fold_array_literals_recurse(stmt: &mut Statement) {
    map_nested_bodies_mut(stmt, fold_array_literals);
}

