use crate::ir::{AssignTarget, Binding, Expression, PropertyKey, Statement, Value};

mod legacy;
mod modern;

pub use legacy::detect_legacy_for_of;
pub use modern::detect_for_of_loops;

// `obj[Symbol.iterator]()` -> Some(obj)
pub(super) fn is_iterator_call(expr: &Expression) -> Option<Expression> {
    if let Expression::Call { callee, arguments } = expr {
        if arguments.is_empty() {
            if let Expression::Member {
                object,
                property: PropertyKey::Computed(computed),
                ..
            } = callee.as_ref()
            {
                if let Expression::Member {
                    object: sym,
                    property: PropertyKey::Ident(p),
                    ..
                } = computed.as_ref()
                {
                    if let Expression::Value(Value::Binding(crate::ir::Binding::Variable(n))) =
                        sym.as_ref()
                    {
                        if n == "Symbol" && p == "iterator" {
                            return Some((**object).clone());
                        }
                    }
                }
            }
        }
    }
    None
}

// Remove the `try { body } catch { iter.return(); throw }` wrapper and the
// trailing `// continue` marker that the iterator lowering leaves behind.
pub(super) fn unwrap_iterator_body(body: &[Statement], _iter_reg: u32) -> Vec<Statement> {
    let inner: Vec<Statement> = if body.len() == 1 {
        match &body[0] {
            Statement::TryCatch { try_body, .. } => try_body.clone(),
            _ => body.to_vec(),
        }
    } else {
        body.to_vec()
    };
    inner
        .into_iter()
        .filter(|s| !matches!(s, Statement::Comment(c) if c == "continue"))
        .collect()
}

// Babel's `_createForOfIteratorHelperLoose(iterable)` plus `while (true)` over
// the stepper is a `for (const value of iterable)`. The Hermes IteratorBegin
// protocol is handled separately; this is the helper shape Discord emits.
pub fn fold_babel_for_of(stmts: Vec<Statement>) -> Vec<Statement> {
    let stmts: Vec<Statement> = stmts.into_iter().map(fold_babel_stmt).collect();
    apply_babel_for_of(stmts)
}

fn fold_babel_stmt(stmt: Statement) -> Statement {
    crate::ir::map_nested_bodies(stmt, fold_babel_for_of)
}

fn apply_babel_for_of(stmts: Vec<Statement>) -> Vec<Statement> {
    use std::collections::HashMap;
    let mut stepper: HashMap<String, Expression> = HashMap::new();
    let mut cursor: HashMap<String, String> = HashMap::new();
    let mut out = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        if let Some((name, iterable)) = for_of_helper_binding(&stmt) {
            stepper.insert(name, iterable);
            out.push(stmt);
            continue;
        }
        if let Some((name, step)) = stepper_call_binding(&stmt, &stepper) {
            cursor.insert(name, step);
            out.push(stmt);
            continue;
        }
        if let Statement::While { condition, body } = &stmt {
            if is_true(condition) {
                if let Some(folded) = babel_while_to_for_of(body, &cursor, &stepper) {
                    out.push(folded);
                    continue;
                }
            }
        }
        out.push(stmt);
    }
    out
}

fn babel_while_to_for_of(
    body: &[Statement],
    cursor: &std::collections::HashMap<String, String>,
    stepper: &std::collections::HashMap<String, Expression>,
) -> Option<Statement> {
    let mut value_name: Option<String> = None;
    let mut iterable: Option<Expression> = None;
    let mut kept = Vec::new();
    for stmt in body {
        if let Some(name) = value_from_cursor(stmt, cursor) {
            if value_name.is_none() {
                value_name = Some(name);
                continue;
            }
        }
        if is_stepper_advance(stmt, cursor) {
            if iterable.is_none() {
                if let Some(step_name) = advanced_stepper(stmt, cursor) {
                    iterable = stepper.get(&step_name).cloned();
                }
            }
            continue;
        }
        kept.push(stmt.clone());
    }
    let variable = value_name?;
    let iterable = iterable?;
    Some(Statement::ForOf {
        variable,
        iterable,
        body: kept,
    })
}

fn is_true(expr: &Expression) -> bool {
    matches!(
        expr,
        Expression::Value(Value::Constant(crate::ir::Constant::Bool(true)))
    )
}

fn bound_name_and_value(stmt: &Statement) -> Option<(String, &Expression)> {
    match stmt {
        Statement::Let { name, value, .. } => Some((name.clone(), value)),
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(name)),
            value,
        } => Some((name.clone(), value)),
        _ => None,
    }
}

fn for_of_helper_binding(stmt: &Statement) -> Option<(String, Expression)> {
    let (name, value) = bound_name_and_value(stmt)?;
    let Expression::Call { callee, arguments } = value else {
        return None;
    };
    let callee_name = simple_callee_name(callee)?;
    if !callee_name.contains("ForOfIterator") || arguments.len() != 1 {
        return None;
    }
    Some((name, arguments[0].clone()))
}

fn stepper_call_binding(
    stmt: &Statement,
    stepper: &std::collections::HashMap<String, Expression>,
) -> Option<(String, String)> {
    let (name, value) = bound_name_and_value(stmt)?;
    let Expression::Call { callee, arguments } = value else {
        return None;
    };
    if !arguments.is_empty() {
        return None;
    }
    let step = simple_callee_name(callee)?;
    if stepper.contains_key(&step) {
        Some((name, step))
    } else {
        None
    }
}

fn simple_callee_name(callee: &Expression) -> Option<String> {
    match callee {
        Expression::Value(Value::Binding(Binding::Variable(n))) => Some(n.clone()),
        Expression::Member {
            property: PropertyKey::Ident(n),
            ..
        } => Some(n.clone()),
        _ => None,
    }
}

fn value_from_cursor(
    stmt: &Statement,
    cursor: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let (name, value) = bound_name_and_value(stmt)?;
    let Expression::Member {
        object,
        property: PropertyKey::Ident(prop),
        ..
    } = value
    else {
        return None;
    };
    if prop != "value" {
        return None;
    }
    let Expression::Value(Value::Binding(Binding::Variable(obj))) = object.as_ref() else {
        return None;
    };
    if cursor.contains_key(obj) {
        Some(name)
    } else {
        None
    }
}

fn is_stepper_advance(
    stmt: &Statement,
    cursor: &std::collections::HashMap<String, String>,
) -> bool {
    advanced_stepper(stmt, cursor).is_some()
}

fn advanced_stepper(
    stmt: &Statement,
    cursor: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let (name, value) = bound_name_and_value(stmt)?;
    let step = cursor.get(&name)?;
    let Expression::Call { callee, arguments } = value else {
        return None;
    };
    if !arguments.is_empty() {
        return None;
    }
    let called = simple_callee_name(callee)?;
    if &called == step {
        Some(step.clone())
    } else {
        None
    }
}

#[cfg(test)]
mod babel_for_of_tests {
    use super::*;
    use crate::ir::VarKind;

    #[test]
    fn helper_while_true_becomes_for_of() {
        let iterable = Expression::Value(Value::Binding(Binding::Variable("items".into())));
        let stmts = vec![
            Statement::Let {
                name: "step".into(),
                value: Expression::Call {
                    callee: Box::new(Expression::Value(Value::Binding(Binding::Variable(
                        "_createForOfIteratorHelperLoose".into(),
                    )))),
                    arguments: vec![iterable.clone()],
                },
                kind: VarKind::Let,
            },
            Statement::Let {
                name: "cursor".into(),
                value: Expression::Call {
                    callee: Box::new(Expression::Value(Value::Binding(Binding::Variable(
                        "step".into(),
                    )))),
                    arguments: vec![],
                },
                kind: VarKind::Let,
            },
            Statement::While {
                condition: Expression::Value(Value::Constant(crate::ir::Constant::Bool(true))),
                body: vec![
                    Statement::Let {
                        name: "value".into(),
                        value: Expression::Member {
                            object: Box::new(Expression::Value(Value::Binding(Binding::Variable(
                                "cursor".into(),
                            )))),
                            property: PropertyKey::Ident("value".into()),
                            optional: false,
                        },
                        kind: VarKind::Let,
                    },
                    Statement::Expr(Expression::Value(Value::Binding(Binding::Variable(
                        "value".into(),
                    )))),
                    Statement::Assign {
                        target: AssignTarget::Binding(Binding::Variable("cursor".into())),
                        value: Expression::Call {
                            callee: Box::new(Expression::Value(Value::Binding(Binding::Variable(
                                "step".into(),
                            )))),
                            arguments: vec![],
                        },
                    },
                ],
            },
        ];
        let out = fold_babel_for_of(stmts);
        assert!(matches!(
            &out[2],
            Statement::ForOf { variable, .. } if variable == "value"
        ));
    }
}
